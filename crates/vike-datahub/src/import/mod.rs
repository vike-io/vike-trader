//! **The ARCHIVE IMPORT lane** — `Request::ImportArchive`'s server half: the imports root, the walk
//! and the safe open ([`walk`]), the format registry's seam ([`ArchiveFormat`]), the one-import
//! slot, the plan, and the verb handler the connection loop dispatches to.
//!
//! `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` is the approved design (task T4
//! of its §9) and
//! `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md` the
//! binding verdicts. In one paragraph: the user syncs a vendor archive under THEIR OWN account into
//! `<project>/market_data/imports/<format>/<dataset>/` on the DATAHUB's box, and this verb reads it
//! into the served store. The request names a format id from this server's registry and ONE
//! validated directory name — never a path — and the server composes the path itself.
//!
//! # What is shared, and what each format owns (design §6)
//!
//! **Shared, here:** the verb, the imports root, the walk and the safe open, the walk's caps and
//! the one-import slot, the plan and outcome report, and the per-format capability string.
//! **The format's own, behind [`ArchiveFormat`]:** the layout grammar, the admission (a price
//! scale), the header, the decode, the store keys and the cross-lane rule with its venue's other
//! lanes. Every vendor's archive disagrees on every one of those, so none is generalised.
//!
//! ⚠ **The trait is FEATURE-FREE, and the store is captured by the format INSTANCE.** The design's
//! §6 sketches a `static` of unit structs whose `import_day` takes `&DataFusionHist`. That type is
//! nameable only under `serve-datafusion`, and this module — like `crate::backfill`'s table — must
//! compile, DECODE the verb and refuse it cleanly on every build. So the registry is a constructor,
//! `formats::real_import_registry(store)`, behind `backfill-serve`, exactly as
//! `crate::backfill::real_backfill_table` is: the ONE place naming a format, folding the concrete
//! store into each instance. A build without it mounts no lane.
//!
//! # The order of a request, cheapest refusal first
//!
//! 1. no lane mounted → the capability refusal ([`NO_IMPORT_LANE`]);
//! 2. the shared validator (`vike_datahub_client::archive::validate_import_spec`) — the SAME one the
//!    client ran, so the refusal an operator reads locally is the one this door gives;
//! 3. the format must be registered here; the dataset must be ADMITTED by it (Dukascopy: a measured
//!    price scale) — before any directory is touched;
//! 4. a request that DECODES (an import, or a `verify` dry run) takes the ONE slot, or is refused;
//! 5. the walk; then the window is RESOLVED (an omitted bound becomes the dataset's own first or last
//!    day) and [`IMPORT_MAX_DAYS`] is checked again — the client could not, without the directory;
//! 6. the store session (an import takes the day-owner lock here, for the whole request), the plan,
//!    and — unless the request was a plan-only dry run — the import or the verify, one day at a time.
//!
//! # A client that goes away stops the import at the next day
//!
//! The verb runs on its connection's thread and nothing reads the socket meanwhile, so the request's
//! stop probe (`crate::server`'s `StopProbe`, the one `Backfill` takes) is asked between days and
//! never inside one. Once it says stop, the request ends at that boundary: every day before it stays
//! stored (each is one locked manifest publish), and the connection closes without a reply.
//! Re-running the same request re-plans and finds those days HELD.

pub mod walk;

#[cfg(feature = "backfill-serve")]
pub mod formats;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use vike_data::SeriesCoverage;
use vike_datahub_client::archive::{
    ArchiveInventory, DatasetDir, DayClass, DayOutcome, DayPlan, DayRefusal, DayResult,
    IMPORT_MAX_DAYS, ImportDone, ImportOutcome, ImportPlan, ImportSpec, validate_import_spec,
};
use vike_datahub_client::proto::{FEATURE_ARCHIVE_IMPORT, Response};
use vike_model::MS_PER_DAY;

use walk::{VettedFile, Walk, WalkCaps};

/// What one FILE's path means under a format's layout grammar — decided from the path alone,
/// nothing opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutFile {
    /// A file of the importable layout, for the UTC day starting at this epoch-ms.
    Daily(i64),
    /// A file of a RECOGNISED layout this format does not import (Dukascopy's hourly files), for the
    /// UTC day starting at this epoch-ms. Counted and reported by day; a day holding both layouts is
    /// refused.
    OtherLayout(i64),
    /// Anything else: counted with its bytes, never opened.
    Other,
}

/// **One archive FORMAT the lane can import** — the design's §6 seam. Everything a format owns goes
/// through here; everything shared stays in this module.
///
/// ⚠ Feature-free, and the store is the INSTANCE's: see the module doc for why the registry is a
/// constructor rather than the sketch's `static`.
pub trait ArchiveFormat: Send + Sync {
    /// The wire id AND the directory name under the imports root — `"dukascopy-bi5"`. Advertised as
    /// `import_format=<id>`.
    fn id(&self) -> &'static str;
    /// The store venue every dataset of this format lands under — decided HERE, never by a request.
    fn venue(&self) -> &'static str;
    /// Admit `dataset` (already validated by the shared validator), or refuse it. `Ok` is the
    /// format's one-line statement of WHY it is admitted — for Dukascopy, the instrument's point
    /// value and the evidence for it; `Err` is a whole-request refusal, given before any directory
    /// is touched.
    fn admit(&self, dataset: &str) -> Result<String, String>;
    /// Whether the walk may descend into a DIRECTORY at `rel` (segments relative to the dataset
    /// directory, at least one). The walk's own depth cap applies on top.
    fn accepts_dir(&self, rel: &[&str], now_ms: i64) -> bool;
    /// What a regular FILE at `rel` is, by name and depth alone.
    fn classify_file(&self, rel: &[&str], now_ms: i64) -> LayoutFile;
    /// The largest file this format will read, in bytes — the `take(cap + 1)` bound.
    fn max_file_bytes(&self) -> u64;
    /// How many leading bytes [`Self::read_header`] needs.
    fn header_len(&self) -> usize;
    /// What a file's header declares, from its first [`Self::header_len`] bytes (fewer when the
    /// file is shorter) and its size — `Ok(Some(ticks))` when the header declares a tick count,
    /// `Ok(None)` when it declares none, `Err` when the header alone already refuses the file (a
    /// declared size over a cap, a file over the size cap). Decodes nothing.
    fn read_header(&self, file_len: u64, prefix: &[u8]) -> Result<Option<u64>, DayRefusal>;
    /// Open the store side of one request over `dataset`'s series. `writes` is `true` for an import
    /// — the session then holds whatever lock the format's cross-lane rule needs for the whole
    /// request — and `false` for a dry run, which must take none.
    fn session<'a>(
        &'a self,
        dataset: &str,
        bars: &[String],
        now_ms: i64,
        writes: bool,
    ) -> Result<Box<dyn ImportSession + 'a>, String>;
}

/// The store side of ONE request: the series' coverage, each day's class, and the per-day import or
/// verify. `read` hands the format the day's file through the lane's safe open; a day that is not
/// decoded must never call it.
pub trait ImportSession {
    /// The series as the store holds it NOW, from any lane; `None` when it holds nothing.
    fn series(&self) -> Option<SeriesCoverage>;
    /// The day's class under the design's §4.2 one-owner table.
    fn classify(&self, day: i64) -> DayClass;
    /// Import the day: store a FREE/SUPERSEDE day, top up a HELD one, refuse the rest. `Err` is not
    /// one day's problem — the store failing — and ends the request.
    fn import_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String>;
    /// Decode the day (an importable one) and check it, writing NOTHING.
    fn verify_day(
        &mut self,
        day: i64,
        read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
    ) -> Result<DayResult, String>;
}

/// The refusal a server with NO import lane answers `ImportArchive` with.
///
/// ⚠ It must keep naming the capability and saying nothing was read:
/// `crates/vike-datahub-client/tests/archive_import_negotiation.rs`'s
/// `a_server_with_no_lane_answers_a_raw_import_frame_with_the_capability_refusal` pins both, and the
/// auth sweeps in `crates/vike-datahub/tests/auth_roundtrip.rs` require it to name no SCOPE (a
/// key-less server is not refusing on scope grounds).
pub const NO_IMPORT_LANE: &str = "ImportArchive: this datahub mounts no archive import lane, so the \
     `archive_import` capability is absent from its Welcome.features. Nothing was read and nothing was \
     written. Its startup log says why: either it has no project directory above it (so no imports \
     root, <project>/market_data/imports), or it was built without `--features backfill-serve`, which \
     carries the format registry.";

/// The refusal a second DECODING request gets while one is running — design §5's "ONE import per
/// datahub; a second is refused, not queued".
pub const IMPORT_BUSY: &str = "ImportArchive: another archive import (or `verify`) is already running \
     on this datahub, and it runs ONE at a time — each holds a whole day's decoded ticks in memory, \
     and two would double that inside one memory cap. Nothing was read and nothing was written; send \
     it again once the running one has answered. (A plan-only dry run decodes nothing and is not held \
     to this.)";

/// The mounted lane: the imports root, the registry, the walk's caps and the one-import slot.
pub struct ImportLane {
    root: PathBuf,
    formats: Vec<Box<dyn ArchiveFormat>>,
    caps: WalkCaps,
    /// The ONE slot — `true` while a decoding request runs.
    busy: AtomicBool,
}

impl ImportLane {
    /// A lane over `root` (the imports root, already resolved) serving `formats`, under the design's
    /// walk caps.
    pub fn new(root: PathBuf, formats: Vec<Box<dyn ArchiveFormat>>) -> Self {
        Self { root, formats, caps: WalkCaps::DEFAULT, busy: AtomicBool::new(false) }
    }

    /// The imports root this lane reads under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The registered format ids, in registry order — what the `import_format=` entries advertise.
    pub fn format_ids(&self) -> Vec<&'static str> {
        self.formats.iter().map(|f| f.id()).collect()
    }

    fn format(&self, id: &str) -> Option<&dyn ArchiveFormat> {
        self.formats.iter().find(|f| f.id() == id).map(|f| f.as_ref())
    }

    /// Take the ONE slot, or `None` while another decoding request holds it.
    fn take_slot(&self) -> Option<Slot<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Slot(&self.busy))
    }
}

impl std::fmt::Debug for ImportLane {
    /// The root and the format ids — the formats themselves hold a store handle and print nothing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportLane")
            .field("root", &self.root)
            .field("formats", &self.format_ids())
            .finish()
    }
}

/// The held slot; dropping it — on every return path, a panic's unwind included — frees it.
struct Slot<'l>(&'l AtomicBool);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The `Welcome.features` entries a lane adds: [`FEATURE_ARCHIVE_IMPORT`] and, beside it, one
/// `import_format=<id>` per registered format — TOGETHER, which is what makes an empty format list
/// an unambiguous refusal on the client (`vike_datahub_client::proto`'s
/// `FEATURE_IMPORT_FORMAT_PREFIX`). Nothing without a lane: the advertisement is a RUNTIME fact,
/// keyed on the mount, never on the build.
pub fn advertised(lane: Option<&ImportLane>) -> Vec<String> {
    let Some(lane) = lane else { return Vec::new() };
    let mut features = vec![FEATURE_ARCHIVE_IMPORT.to_string()];
    features.extend(
        lane.format_ids().into_iter().map(vike_datahub_client::proto::import_format_feature),
    );
    features
}

/// Decide the lane at startup, and say what was decided — `(lane, the startup line)`.
///
/// - **No format** (a build without `backfill-serve`) → no lane: a lane advertising
///   `archive_import` with nothing to import would be a capability it can only refuse.
/// - **No project above the daemon** (`settings_dir` absent, or one with no parent) → no lane, the
///   design's §2.3 rule: there is no imports root to read.
/// - Otherwise the lane mounts over `<project>/market_data/imports`, CANONICALIZED here, once, so a
///   symlink or a mount AT the root — the operator's way to put it on another disk — is resolved at
///   startup rather than per request. A root that does not exist YET mounts anyway, under its
///   composed path: every dataset then plans as absent, naming where the files must go, and the
///   first sync creates it. A root that exists and cannot be resolved (a unit whose
///   `ProtectHome=yes` hides a root symlinked into a home directory) mounts the same way and plans
///   as unreadable — "exists, cannot be read" rather than "absent".
pub fn mount(
    settings_dir: Option<&Path>,
    formats: Vec<Box<dyn ArchiveFormat>>,
) -> (Option<ImportLane>, String) {
    if formats.is_empty() {
        return (
            None,
            "ARCHIVE IMPORT lane NOT mounted — this build carries no archive format (the registry is \
             behind `--features backfill-serve`), so ImportArchive is refused by name and \
             `archive_import` is not advertised"
                .to_string(),
        );
    }
    let Some(root) = vike_model::paths::state_path::imports_dir_beside(settings_dir) else {
        return (
            None,
            "ARCHIVE IMPORT lane NOT mounted — this daemon has no project directory above it, so it \
             has no imports root (<project>/market_data/imports). ImportArchive is refused by name \
             and `archive_import` is not advertised; run it from a project, or set \
             VIKE_SETTINGS_DIR"
                .to_string(),
        );
    };
    let (root, state) = match std::fs::canonicalize(&root) {
        Ok(resolved) => (resolved, "present".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (root, "not created yet — the first sync into it creates it".to_string())
        }
        Err(e) => (
            root,
            format!("present but not resolvable ({}) — imports will plan as unreadable", e.kind()),
        ),
    };
    let lane = ImportLane::new(root, formats);
    let line = format!(
        "ARCHIVE IMPORT lane mounted — imports root {} ({state}); formats [{}]. ImportArchive \
         (Control scope) reads ONLY files under this root, on THIS box",
        lane.root.display(),
        lane.format_ids().join(", ")
    );
    (Some(lane), line)
}

/// **The `ImportArchive` verb** — see the module doc for the order of a request.
///
/// `should_stop` is the request's stop probe: asked between days, never inside one. Once it has
/// answered `true` the request ends at that boundary and the `Response` returned is never written —
/// the connection loop closes on the probe's latch, exactly as for a stopped `Backfill`.
pub fn import_archive_verb(
    spec: &ImportSpec,
    lane: Option<&ImportLane>,
    should_stop: &dyn Fn() -> bool,
) -> Response {
    let Some(lane) = lane else {
        return Response::Error(NO_IMPORT_LANE.to_string());
    };
    // The SAME validator the client ran — the server copy is the enforcement.
    if let Err(why) = validate_import_spec(spec) {
        return Response::Error(format!("ImportArchive: {why}"));
    }
    // ⚠ The format string is the CLIENT's and is not echoed: the refusal names what IS registered,
    // which is what the sender can act on.
    let Some(format) = lane.format(&spec.format) else {
        return Response::Error(format!(
            "ImportArchive: this datahub registers no archive format by that name. It registers \
             [{}]. Nothing was read and nothing was written.",
            lane.format_ids().join(", ")
        ));
    };
    let admission = match format.admit(&spec.dataset) {
        Ok(admission) => admission,
        Err(why) => {
            return Response::Error(format!(
                "ImportArchive {}: {why} Nothing was read and nothing was written.",
                format.id()
            ));
        }
    };
    // THE ONE SLOT — held to the end of this function, released by its `Drop` on every path.
    let _slot = if spec.decodes() {
        match lane.take_slot() {
            Some(slot) => Some(slot),
            None => return Response::Error(IMPORT_BUSY.to_string()),
        }
    } else {
        None
    };
    run(lane, format, spec, admission, vike_model::now_ms(), should_stop)
}

/// Steps 5 and 6 of the module doc's order — everything after the slot.
fn run(
    lane: &ImportLane,
    format: &dyn ArchiveFormat,
    spec: &ImportSpec,
    admission: String,
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Response {
    let server_dir = lane.root.join(format.id()).join(&spec.dataset).display().to_string();
    let walk = match walk::walk_dataset(&lane.root, format, &spec.dataset, now_ms, &lane.caps) {
        Ok(walk) => walk,
        Err(why) => return Response::Error(format!("ImportArchive {}: {why}", format.id())),
    };
    let inventory = inventory_of(&walk);
    let from = spec.from_day.or(inventory.first_day);
    let to = spec.to_day.or(inventory.last_day);
    let mut plan = ImportPlan {
        format: format.id().to_string(),
        dataset: spec.dataset.clone(),
        venue: format.venue().to_string(),
        server_dir,
        dir: walk.dir.clone(),
        admission,
        inventory,
        from_day: from,
        to_day: to,
        days: Vec::new(),
        gaps: Vec::new(),
        bars: spec.bars.clone(),
        series: None,
    };
    let outcome_if_any = || spec.decodes().then(ImportOutcome::default);
    if walk.dir != DatasetDir::Present {
        return done(plan, outcome_if_any());
    }
    // THE RE-CHECK AFTER RESOLVING. The client's validator applied the day cap only where it knew
    // both bounds; an omitted one is the dataset's own first or last day, which only this side of
    // the wire can see.
    if let (Some(from), Some(to)) = (from, to)
        && spec.decodes()
        && from <= to
    {
        let days = to.saturating_sub(from) / MS_PER_DAY + 1;
        if days > IMPORT_MAX_DAYS {
            return Response::Error(format!(
                "ImportArchive {}: with its omitted bound(s) filled in from the dataset, this request \
                 would decode {days} days, over IMPORT_MAX_DAYS = {IMPORT_MAX_DAYS}: one request is \
                 synchronous and this wire carries no progress frame. Send explicit bounds and split \
                 the window by calendar month (a plan-only dry run is not bounded by this cap). \
                 Nothing was read and nothing was written.",
                format.id()
            ));
        }
    }
    let mut session = match format.session(&spec.dataset, &spec.bars, now_ms, spec.writes()) {
        Ok(session) => session,
        Err(why) => return Response::Error(format!("ImportArchive {}: {why}", format.id())),
    };
    plan.series = session.series();

    // ---- the plan: every day of the window that HAS a daily file --------------------------------
    let window: Vec<(i64, &VettedFile)> = match (from, to) {
        (Some(from), Some(to)) if from <= to => {
            walk.daily.range(from..=to).map(|(day, file)| (*day, file)).collect()
        }
        _ => Vec::new(),
    };
    for (day, file) in &window {
        if should_stop() {
            return stopped(format, spec, "while planning", 0);
        }
        plan.days.push(plan_day(format, &walk, session.as_ref(), *day, file));
    }
    plan.gaps = gaps(&walk, &plan.inventory, from, to);
    tracing::info!(
        format = format.id(),
        dataset = %spec.dataset,
        days = plan.days.len(),
        importable = plan.importable_days(),
        dry_run = spec.dry_run,
        verify = spec.verify,
        "vike-datahub: ImportArchive planned"
    );
    if !spec.decodes() {
        return done(plan, None);
    }

    // ---- the decode half: an import, or a verify — one day per step ------------------------------
    let cap = format.max_file_bytes();
    let mut outcome = ImportOutcome::default();
    for (planned, (day, file)) in plan.days.iter().zip(&window) {
        debug_assert_eq!(planned.day, *day);
        if should_stop() {
            return stopped(format, spec, "between days", outcome.days.len());
        }
        let mut read = || walk::read_vetted(file, cap);
        let result = if spec.writes() {
            match &planned.class {
                // A plan-time refusal (a mixed-layout day, a header past a cap) is never handed to
                // the store: the store half would classify such a day FREE and decode it.
                DayClass::Refused(refusal) => Ok(DayResult::Refused(refusal.clone())),
                _ => session.import_day(*day, &mut read),
            }
        } else if planned.class.is_importable() {
            session.verify_day(*day, &mut read)
        } else {
            // A verify decodes exactly what the import would: the importable days.
            continue;
        };
        match result {
            Ok(result) => outcome.days.push(DayOutcome { day: *day, result }),
            Err(why) => {
                return Response::Error(format!(
                    "ImportArchive {} {}: the store failed at day {day}: {why}. Every day before it \
                     is stored and stays so; repeating the request resumes there.",
                    format.id(),
                    spec.dataset
                ));
            }
        }
    }
    drop(session);
    tracing::info!(
        format = format.id(),
        dataset = %spec.dataset,
        days = outcome.days.len(),
        refused = outcome.days.iter().filter(|d| matches!(d.result, DayResult::Refused(_))).count(),
        verify = spec.verify,
        "vike-datahub: ImportArchive finished"
    );
    done(plan, Some(outcome))
}

/// One day of the plan: the mixed-layout refusal first (the vendor: never mix the two layouts),
/// then the store's class, then — for an importable day only — what its header declares. A HELD or
/// refused day's file is never opened.
fn plan_day(
    format: &dyn ArchiveFormat,
    walk: &Walk,
    session: &dyn ImportSession,
    day: i64,
    file: &VettedFile,
) -> DayPlan {
    let mut class = if walk.other_layout.contains(&day) {
        DayClass::Refused(DayRefusal {
            class: MIXED_LAYOUT.to_string(),
            detail: format!(
                "this day holds a daily file AND files of the other layout, and the vendor's own rule \
                 is never to mix the two. Remove one layout's files for the day (`{}` is the daily \
                 one). Nothing was read and no commit key was spent.",
                file.rel
            ),
        })
    } else {
        session.classify(day)
    };
    let mut declared_ticks = None;
    if class.is_importable() {
        let header = walk::read_vetted_prefix(file, format.header_len())
            .and_then(|prefix| format.read_header(file.len, &prefix));
        match header {
            Ok(declared) => declared_ticks = declared,
            Err(refusal) => class = DayClass::Refused(refusal),
        }
    }
    DayPlan { day, class, file_bytes: file.len, declared_ticks }
}

/// The class a day holding BOTH layouts is refused under.
pub const MIXED_LAYOUT: &str = "MixedLayout";

/// The walk's findings over the WHOLE dataset, whatever the window.
fn inventory_of(walk: &Walk) -> ArchiveInventory {
    ArchiveInventory {
        daily_files: walk.daily.len() as u64,
        first_day: walk.daily.keys().next().copied(),
        last_day: walk.daily.keys().next_back().copied(),
        // A day holding both layouts is a refused PLAN day, not an other-layout day.
        other_layout_days: walk
            .other_layout
            .iter()
            .copied()
            .filter(|day| !walk.daily.contains_key(day))
            .collect(),
        other_objects: walk.other_objects,
        other_bytes: walk.other_bytes,
        skipped: walk.skipped.clone(),
    }
}

/// Weekdays inside BOTH the resolved window and the dataset's own span that hold no file of either
/// layout. Bounded by the dataset's span, which comes from real files, so a window of any width
/// costs at most the dataset's own days.
fn gaps(walk: &Walk, inventory: &ArchiveInventory, from: Option<i64>, to: Option<i64>) -> Vec<i64> {
    let (Some(first), Some(last), Some(from), Some(to)) =
        (inventory.first_day, inventory.last_day, from, to)
    else {
        return Vec::new();
    };
    let (start, end) = (from.max(first), to.min(last));
    let mut gaps = Vec::new();
    let mut day = start;
    while day <= end {
        if is_weekday(day) && !walk.daily.contains_key(&day) && !walk.other_layout.contains(&day) {
            gaps.push(day);
        }
        day += MS_PER_DAY;
    }
    gaps
}

/// Monday to Friday, UTC. 1970-01-01 was a Thursday, so the day index plus three, mod seven, counts
/// from Monday = 0.
fn is_weekday(day: i64) -> bool {
    (day.div_euclid(MS_PER_DAY) + 3).rem_euclid(7) < 5
}

fn done(plan: ImportPlan, outcome: Option<ImportOutcome>) -> Response {
    Response::ArchiveImported(Box::new(ImportDone { plan, outcome }))
}

/// The request's client has gone: one line at the boundary, and an answer the connection loop never
/// writes (it closes on the probe's latch).
fn stopped(
    format: &dyn ArchiveFormat,
    spec: &ImportSpec,
    when: &str,
    days_done: usize,
) -> Response {
    tracing::info!(
        format = format.id(),
        dataset = %spec.dataset,
        days_done,
        "vike-datahub: ImportArchive STOPPED {when} — its client closed the connection, so no reply \
         is written and the connection closes. Every day finished before the boundary stays \
         stored; repeating the request resumes there"
    );
    Response::Error(format!(
        "ImportArchive {} {}: stopped {when} — its client closed the connection ({days_done} days \
         done; repeating the request resumes).",
        format.id(),
        spec.dataset
    ))
}

#[cfg(test)]
mod tests {
    //! The lane's own rules, over a FAKE format and real temporary directories — the default build,
    //! so the roster lane runs them on every PR. The real Dukascopy format, a real store and the
    //! real wire are `crates/vike-datahub/tests/archive_import.rs`'s, behind `backfill-serve`.

    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use super::*;

    const FAKE: &str = "fake-days";
    const DAY: i64 = MS_PER_DAY;
    /// 2024-01-15, a Monday.
    const MON: i64 = 19_737 * DAY;
    const WAIT: Duration = Duration::from_secs(10);

    /// The fake grammar: any all-digit directory name at any depth (so the WALK's depth cap is the
    /// only thing that bounds the descent), a file named `<n>.day` is the daily file of day `n`, and
    /// `<n>.hour` a file of the other layout for day `n`. Everything else is "other".
    struct FakeFormat {
        /// What the store side does, shared with the test.
        store: Arc<FakeStore>,
    }

    /// The fake store side: classes by day (FREE unless named), and a record of every decoded day.
    #[derive(Default)]
    struct FakeStore {
        classes: Mutex<BTreeMap<i64, DayClass>>,
        imported: Mutex<Vec<(i64, Vec<u8>)>>,
        sessions: AtomicUsize,
        /// When set, the FIRST `import_day` reports on `.0` and then waits on `.1` — the hook the
        /// slot test parks a running import on.
        park: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    }

    impl FakeFormat {
        fn new() -> (Self, Arc<FakeStore>) {
            let store = Arc::new(FakeStore::default());
            (FakeFormat { store: Arc::clone(&store) }, store)
        }
    }

    fn fake_day(name: &str, suffix: &str) -> Option<i64> {
        let n: i64 = name.strip_suffix(suffix)?.parse().ok()?;
        Some(n * DAY)
    }

    impl ArchiveFormat for FakeFormat {
        fn id(&self) -> &'static str {
            FAKE
        }
        fn venue(&self) -> &'static str {
            "fakevenue"
        }
        fn admit(&self, dataset: &str) -> Result<String, String> {
            if dataset == "UNSCALED" {
                Err("no scale.".to_string())
            } else {
                Ok("fake".to_string())
            }
        }
        fn accepts_dir(&self, rel: &[&str], _now_ms: i64) -> bool {
            rel.last().is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        fn classify_file(&self, rel: &[&str], _now_ms: i64) -> LayoutFile {
            let name = rel.last().copied().unwrap_or_default();
            if let Some(day) = fake_day(name, ".day") {
                LayoutFile::Daily(day)
            } else if let Some(day) = fake_day(name, ".hour") {
                LayoutFile::OtherLayout(day)
            } else {
                LayoutFile::Other
            }
        }
        fn max_file_bytes(&self) -> u64 {
            64
        }
        fn header_len(&self) -> usize {
            1
        }
        fn read_header(&self, _file_len: u64, prefix: &[u8]) -> Result<Option<u64>, DayRefusal> {
            match prefix.first() {
                Some(b'X') => Err(DayRefusal { class: "BadHeader".into(), detail: "x".into() }),
                Some(n) => Ok(Some(u64::from(*n))),
                None => Ok(Some(0)),
            }
        }
        fn session<'a>(
            &'a self,
            _dataset: &str,
            _bars: &[String],
            _now_ms: i64,
            _writes: bool,
        ) -> Result<Box<dyn ImportSession + 'a>, String> {
            self.store.sessions.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeSession { store: &self.store }))
        }
    }

    struct FakeSession<'a> {
        store: &'a FakeStore,
    }

    impl ImportSession for FakeSession<'_> {
        fn series(&self) -> Option<SeriesCoverage> {
            None
        }
        fn classify(&self, day: i64) -> DayClass {
            self.store.classes.lock().unwrap().get(&day).cloned().unwrap_or(DayClass::Free)
        }
        fn import_day(
            &mut self,
            day: i64,
            read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
        ) -> Result<DayResult, String> {
            if let Some((started, release)) = self.store.park.lock().unwrap().take() {
                started.send(()).unwrap();
                release.recv_timeout(WAIT).map_err(|_| "the test never released".to_string())?;
            }
            if !self.classify(day).is_importable() {
                return Ok(DayResult::ToppedUp { bars: Vec::new() });
            }
            match read() {
                Ok(bytes) => {
                    let ticks = bytes.len() as u64;
                    self.store.imported.lock().unwrap().push((day, bytes));
                    Ok(DayResult::Imported { ticks, bars: Vec::new() })
                }
                Err(refusal) => Ok(DayResult::Refused(refusal)),
            }
        }
        fn verify_day(
            &mut self,
            _day: i64,
            read: &mut dyn FnMut() -> Result<Vec<u8>, DayRefusal>,
        ) -> Result<DayResult, String> {
            Ok(match read() {
                Ok(bytes) => DayResult::Verified { ticks: bytes.len() as u64 },
                Err(refusal) => DayResult::Refused(refusal),
            })
        }
    }

    /// An imports root with the fake format's directory and one dataset, `EURUSD`.
    struct Tree {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    impl Tree {
        fn new() -> Tree {
            let tmp = tempfile::tempdir().expect("a temp dir");
            let root = tmp.path().join("imports");
            fs::create_dir_all(root.join(FAKE).join("EURUSD")).unwrap();
            Tree { _tmp: tmp, root }
        }
        fn dataset(&self) -> PathBuf {
            self.root.join(FAKE).join("EURUSD")
        }
        /// A daily file for `day` under the bucket directory `bucket`, holding `bytes`.
        fn daily(&self, day: i64, bytes: &[u8]) -> PathBuf {
            let dir = self.dataset().join(format!("{}", day / DAY / 100));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{}.day", day / DAY));
            fs::write(&path, bytes).unwrap();
            path
        }
        fn lane(&self) -> (ImportLane, Arc<FakeStore>) {
            let (format, store) = FakeFormat::new();
            (ImportLane::new(self.root.clone(), vec![Box::new(format)]), store)
        }
    }

    fn spec(from: Option<i64>, to: Option<i64>, dry_run: bool, verify: bool) -> ImportSpec {
        ImportSpec {
            format: FAKE.to_string(),
            dataset: "EURUSD".to_string(),
            from_day: from,
            to_day: to,
            bars: Vec::new(),
            dry_run,
            verify,
        }
    }

    fn never() -> bool {
        false
    }

    fn done_of(response: Response) -> ImportDone {
        match response {
            Response::ArchiveImported(done) => *done,
            other => panic!("expected ArchiveImported, got {other:?}"),
        }
    }

    fn error_of(response: Response) -> String {
        match response {
            Response::Error(msg) => msg,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    fn walk_of(tree: &Tree, caps: &WalkCaps) -> Walk {
        let (format, _) = FakeFormat::new();
        walk::walk_dataset(&tree.root, &format, "EURUSD", 0, caps).expect("under the caps")
    }

    // ---- the advertisement ------------------------------------------------------------------------

    /// The capability and one `import_format=` entry per format, together — and NOTHING without a
    /// lane. A lane is a runtime fact: this is what `served_features` pushes.
    #[test]
    fn the_advertisement_appears_exactly_when_a_lane_is_mounted() {
        assert!(advertised(None).is_empty(), "no lane, no capability and no format");
        let tree = Tree::new();
        let (lane, _) = tree.lane();
        let features = advertised(Some(&lane));
        assert_eq!(
            features,
            vec![
                FEATURE_ARCHIVE_IMPORT.to_string(),
                vike_datahub_client::proto::import_format_feature(FAKE),
            ]
        );
        assert_eq!(vike_datahub_client::proto::advertised_import_formats(&features), vec![FAKE]);
    }

    /// No format, no lane; no project, no lane; a project mounts one even before its root exists.
    #[test]
    fn the_lane_mounts_only_with_a_format_and_a_project() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings");
        let (lane, line) = mount(Some(&settings), Vec::new());
        assert!(lane.is_none() && line.contains("backfill-serve"), "{line}");

        let (format, _) = FakeFormat::new();
        let (lane, line) = mount(None, vec![Box::new(format)]);
        assert!(lane.is_none() && line.contains("no project directory"), "{line}");

        let (format, _) = FakeFormat::new();
        let (lane, line) = mount(Some(&settings), vec![Box::new(format)]);
        let lane = lane.expect("a project mounts the lane");
        assert_eq!(lane.root(), tmp.path().join("market_data").join("imports"));
        assert!(line.contains("not created yet"), "{line}");

        // Once the root exists it is CANONICALIZED at mount.
        fs::create_dir_all(tmp.path().join("market_data").join("imports")).unwrap();
        let (format, _) = FakeFormat::new();
        let (lane, _) = mount(Some(&settings), vec![Box::new(format)]);
        let canonical = fs::canonicalize(tmp.path().join("market_data").join("imports")).unwrap();
        assert_eq!(lane.unwrap().root(), canonical);
    }

    // ---- the door ---------------------------------------------------------------------------------

    #[test]
    fn no_lane_answers_the_capability_refusal_naming_no_scope() {
        let msg = error_of(import_archive_verb(&spec(None, None, true, false), None, &never));
        assert!(msg.contains(FEATURE_ARCHIVE_IMPORT) && msg.contains("Nothing was read"), "{msg}");
        assert!(!msg.contains("scope"), "a lane-less server refuses on no scope grounds: {msg}");
    }

    /// The shared validator runs at THIS door too — a raw frame that skipped the client is refused,
    /// and nothing is walked.
    #[test]
    fn the_server_runs_the_shared_validator_before_touching_the_directory() {
        let tree = Tree::new();
        let (lane, store) = tree.lane();
        for bad in ["..", "EUR/USD", "/ETC", "C:", ".HIDDEN", "EUR\0USD", "eurusd", "CON"] {
            let mut s = spec(None, None, true, false);
            s.dataset = bad.to_string();
            let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
            assert!(msg.contains("import dataset"), "{bad:?}: {msg}");
        }
        let mut s = spec(None, None, true, false);
        s.dataset = "A".repeat(33);
        assert!(error_of(import_archive_verb(&s, Some(&lane), &never)).contains("32"));
        assert_eq!(store.sessions.load(Ordering::SeqCst), 0, "no request reached the store side");
    }

    #[test]
    fn an_unregistered_format_is_refused_naming_the_registered_ones_and_never_echoed() {
        let tree = Tree::new();
        let (lane, _) = tree.lane();
        let mut s = spec(None, None, true, false);
        s.format = "../../etc".to_string();
        let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
        assert!(msg.contains(FAKE) && !msg.contains("etc"), "{msg}");
    }

    #[test]
    fn an_unadmitted_dataset_is_refused_before_anything_is_walked_or_opened() {
        let tree = Tree::new();
        let (lane, store) = tree.lane();
        let mut s = spec(None, None, false, false);
        s.dataset = "UNSCALED".to_string();
        let msg = error_of(import_archive_verb(&s, Some(&lane), &never));
        assert!(msg.contains("no scale") && msg.contains("Nothing was read"), "{msg}");
        assert_eq!(store.sessions.load(Ordering::SeqCst), 0);
    }

    // ---- the plan ---------------------------------------------------------------------------------

    #[test]
    fn an_absent_dataset_plans_as_absent_and_names_the_servers_directory() {
        let tree = Tree::new();
        let (lane, _) = tree.lane();
        let mut s = spec(None, None, true, false);
        s.dataset = "GBPUSD".to_string();
        let done = done_of(import_archive_verb(&s, Some(&lane), &never));
        assert_eq!(done.plan.dir, DatasetDir::Absent);
        assert!(done.plan.server_dir.ends_with("GBPUSD"), "{}", done.plan.server_dir);
        assert!(done.plan.days.is_empty() && done.outcome.is_none());
    }

    /// The window, the gaps, the mixed-layout refusal, the header read and the plan-only answer.
    #[test]
    fn a_dry_run_plans_every_day_with_a_file_and_writes_nothing() {
        let tree = Tree::new();
        // Mon..Fri of one week; Wednesday is missing (a gap); Thursday holds both layouts.
        tree.daily(MON, b"\x05abc");
        tree.daily(MON + DAY, b"\x07abcdef");
        tree.daily(MON + 3 * DAY, b"\x01");
        fs::write(
            tree.dataset()
                .join(format!("{}", (MON + 3 * DAY) / DAY / 100))
                .join(format!("{}.hour", (MON + 3 * DAY) / DAY)),
            b"h",
        )
        .unwrap();
        tree.daily(MON + 4 * DAY, b"X-bad-header");
        fs::write(tree.dataset().join("README.txt"), b"hello").unwrap();
        let (lane, store) = tree.lane();
        store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByArchive);

        let done =
            done_of(import_archive_verb(&spec(None, None, true, false), Some(&lane), &never));
        let plan = &done.plan;
        assert!(done.outcome.is_none(), "a plan-only dry run has no outcome");
        assert_eq!((plan.from_day, plan.to_day), (Some(MON), Some(MON + 4 * DAY)), "resolved");
        assert_eq!(plan.inventory.daily_files, 4);
        assert_eq!((plan.inventory.other_objects, plan.inventory.other_bytes), (1, 5));
        assert!(plan.inventory.other_layout_days.is_empty(), "a mixed day is a refused plan day");
        assert_eq!(plan.gaps, vec![MON + 2 * DAY], "Wednesday has no file");
        let classes: Vec<_> =
            plan.days.iter().map(|d| (d.day, d.class.clone(), d.declared_ticks)).collect();
        assert_eq!(classes[0], (MON, DayClass::Free, Some(5)), "the header was read");
        assert_eq!(
            classes[1],
            (MON + DAY, DayClass::HeldByArchive, None),
            "a held day's file is not opened"
        );
        assert!(matches!(&classes[2].1, DayClass::Refused(r) if r.class == MIXED_LAYOUT));
        assert!(matches!(&classes[3].1, DayClass::Refused(r) if r.class == "BadHeader"));
        assert_eq!(plan.importable_days(), 1);
        assert!(store.imported.lock().unwrap().is_empty(), "a dry run decodes and stores nothing");
    }

    /// The day cap is checked AGAIN once an omitted bound is filled in from the dataset — the case
    /// the client cannot see.
    #[test]
    fn a_32_day_execute_with_omitted_bounds_is_refused_naming_the_cap() {
        let tree = Tree::new();
        tree.daily(MON, b"\x01");
        tree.daily(MON + 31 * DAY, b"\x01");
        let (lane, store) = tree.lane();
        for (from, to) in [(None, None), (Some(MON), None), (None, Some(MON + 31 * DAY))] {
            let msg =
                error_of(import_archive_verb(&spec(from, to, false, false), Some(&lane), &never));
            assert!(msg.contains("IMPORT_MAX_DAYS = 31") && msg.contains("32 days"), "{msg}");
        }
        // ...and a verify is held to it too, while a plan-only dry run is not.
        let msg = error_of(import_archive_verb(&spec(None, None, true, true), Some(&lane), &never));
        assert!(msg.contains("IMPORT_MAX_DAYS"), "{msg}");
        assert_eq!(store.sessions.load(Ordering::SeqCst), 0, "refused before the store session");
        let done =
            done_of(import_archive_verb(&spec(None, None, true, false), Some(&lane), &never));
        assert_eq!(done.plan.days.len(), 2);
        // 31 days is the cap itself, and passes.
        let ok = import_archive_verb(
            &spec(None, Some(MON + 30 * DAY), false, false),
            Some(&lane),
            &never,
        );
        assert_eq!(done_of(ok).outcome.unwrap().days.len(), 1);
    }

    // ---- the import -------------------------------------------------------------------------------

    #[test]
    fn an_import_decodes_the_free_days_tops_up_the_held_ones_and_echoes_plan_refusals() {
        let tree = Tree::new();
        tree.daily(MON, b"\x05abc");
        tree.daily(MON + DAY, b"\x05abc");
        tree.daily(MON + 2 * DAY, b"X");
        let (lane, store) = tree.lane();
        store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByHttp);
        let done =
            done_of(import_archive_verb(&spec(None, None, false, false), Some(&lane), &never));
        let results: Vec<_> =
            done.outcome.unwrap().days.into_iter().map(|d| (d.day, d.result)).collect();
        assert_eq!(results[0], (MON, DayResult::Imported { ticks: 4, bars: Vec::new() }));
        assert_eq!(results[1], (MON + DAY, DayResult::ToppedUp { bars: Vec::new() }));
        assert!(matches!(&results[2].1, DayResult::Refused(r) if r.class == "BadHeader"));
        let imported = store.imported.lock().unwrap();
        assert_eq!(
            imported.as_slice(),
            &[(MON, b"\x05abc".to_vec())],
            "only the FREE day was read"
        );
    }

    #[test]
    fn a_verify_decodes_only_the_importable_days_and_stores_nothing() {
        let tree = Tree::new();
        tree.daily(MON, b"\x05abc");
        tree.daily(MON + DAY, b"\x05abc");
        let (lane, store) = tree.lane();
        store.classes.lock().unwrap().insert(MON + DAY, DayClass::HeldByArchive);
        let done = done_of(import_archive_verb(&spec(None, None, true, true), Some(&lane), &never));
        let days = done.outcome.expect("a verify has an outcome").days;
        assert_eq!(days, vec![DayOutcome { day: MON, result: DayResult::Verified { ticks: 4 } }]);
        assert!(store.imported.lock().unwrap().is_empty());
    }

    /// A file that GREW past the format's cap after the walk — appended to in place, so it is still
    /// the object the walk vetted — is refused at the read, having buffered at most one byte past
    /// the cap. Unix: elsewhere the weaker identity counts a changed length as a different file.
    #[cfg(unix)]
    #[test]
    fn a_file_over_the_formats_cap_at_read_time_is_refused() {
        let tree = Tree::new();
        let path = tree.daily(MON, b"\x05abc");
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        let file = &w.daily[&MON];
        fs::write(&path, vec![b'\x05'; 65]).unwrap(); // same inode, now one byte over the 64 cap
        let refusal = walk::read_vetted(file, 64).expect_err("over the cap");
        assert_eq!(refusal.class, walk::FILE_TOO_LARGE);
        assert!(walk::read_vetted(file, 65).is_ok(), "at the cap it reads");
    }

    // ---- the slot and the stop probe --------------------------------------------------------------

    /// ONE decoding request at a time: a second import while one runs is REFUSED (not queued), a
    /// plan-only dry run is not held to the slot, and the slot frees when the first one answers.
    #[test]
    fn a_second_concurrent_import_is_refused_and_the_slot_frees_afterwards() {
        let tree = Tree::new();
        tree.daily(MON, b"\x05abc");
        let (lane, store) = tree.lane();
        let lane = Arc::new(lane);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *store.park.lock().unwrap() = Some((started_tx, release_rx));

        let first = {
            let lane = Arc::clone(&lane);
            std::thread::spawn(move || {
                import_archive_verb(&spec(None, None, false, false), Some(&lane), &never)
            })
        };
        started_rx.recv_timeout(WAIT).expect("the first import is running");
        let second = import_archive_verb(&spec(None, None, false, false), Some(&lane), &never);
        assert_eq!(error_of(second), IMPORT_BUSY);
        let verify = import_archive_verb(&spec(None, None, true, true), Some(&lane), &never);
        assert_eq!(error_of(verify), IMPORT_BUSY, "a verify decodes, so it is held to the slot");
        let plan = import_archive_verb(&spec(None, None, true, false), Some(&lane), &never);
        assert!(done_of(plan).outcome.is_none(), "a plan-only dry run is not held to the slot");

        release_tx.send(()).unwrap();
        let first = done_of(first.join().expect("the first import answers"));
        assert_eq!(first.outcome.unwrap().days.len(), 1);
        let again = import_archive_verb(&spec(None, None, false, false), Some(&lane), &never);
        assert!(done_of(again).outcome.is_some(), "the slot freed when the first answered");
    }

    /// A client that goes away stops the import AT A DAY BOUNDARY: the days before it are done, the
    /// days after it are never read.
    #[test]
    fn a_stop_probe_that_fires_stops_the_import_at_the_next_day() {
        let tree = Tree::new();
        for i in 0..10 {
            tree.daily(MON + i * DAY, b"\x05abc");
        }
        let (lane, store) = tree.lane();
        let asked = AtomicUsize::new(0);
        // The plan asks once per day (10), then the import asks before each day: stop before the
        // fourth decoded day.
        let should_stop = || asked.fetch_add(1, Ordering::SeqCst) >= 10 + 3;
        let msg = error_of(import_archive_verb(
            &spec(None, None, false, false),
            Some(&lane),
            &should_stop,
        ));
        assert!(msg.contains("stopped between days") && msg.contains("3 days done"), "{msg}");
        let imported: Vec<i64> = store.imported.lock().unwrap().iter().map(|(d, _)| *d).collect();
        assert_eq!(imported, vec![MON, MON + DAY, MON + 2 * DAY], "three days, then the boundary");
    }

    // ---- the walk ---------------------------------------------------------------------------------

    /// The walk never descends past the depth cap, whatever the grammar accepts.
    #[test]
    fn the_walk_stops_at_the_depth_cap() {
        let tree = Tree::new();
        let deep = tree.dataset().join("1").join("2").join("3").join("4");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join(format!("{}.day", MON / DAY)), b"\x01").unwrap();
        fs::write(
            tree.dataset().join("1").join("2").join("3").join(format!("{}.day", (MON + DAY) / DAY)),
            b"\x01",
        )
        .unwrap();
        let w = walk_of(&tree, &WalkCaps::DEFAULT);
        assert_eq!(
            w.daily.keys().copied().collect::<Vec<_>>(),
            vec![MON + DAY],
            "depth 4 found, depth 5 never"
        );
        assert_eq!(w.other_objects, 1, "the depth-4 directory is counted, not descended");
    }

    /// One entry past the cap REFUSES the request rather than planning part of the directory.
    #[test]
    fn a_walk_past_the_entry_cap_refuses_the_request() {
        let tree = Tree::new();
        for i in 0..5 {
            tree.daily(MON + i * DAY, b"\x01");
        }
        // 1 bucket directory + 5 files = 6 entries.
        let caps = WalkCaps { max_entries: 6, max_depth: 4 };
        let (format, _) = FakeFormat::new();
        assert!(walk::walk_dataset(&tree.root, &format, "EURUSD", 0, &caps).is_ok(), "at the cap");
        let caps = WalkCaps { max_entries: 5, max_depth: 4 };
        let why =
            walk::walk_dataset(&tree.root, &format, "EURUSD", 0, &caps).expect_err("one past");
        assert!(why.contains("more than 5 entries") && why.contains("Nothing was read"), "{why}");
        assert_eq!(
            WalkCaps::DEFAULT,
            WalkCaps { max_entries: 100_000, max_depth: 4 },
            "§5's values"
        );
    }

    // ---- confinement (unix): each planted object is refused and never read ------------------------

    #[cfg(unix)]
    mod confinement {
        use std::os::unix::fs::symlink;
        use std::process::Command;

        use vike_datahub_client::archive::EntryClass;

        use super::*;

        /// A directory OUTSIDE the imports root holding a valid-looking daily file — what every
        /// planted link below points at.
        fn outside(tree: &Tree) -> PathBuf {
            let out = tree._tmp.path().join("outside");
            fs::create_dir_all(out.join("2")).unwrap();
            fs::write(out.join("2").join(format!("{}.day", MON / DAY)), b"\x09SECRET").unwrap();
            out
        }

        #[test]
        fn a_symlinked_dataset_is_unreadable_and_never_walked() {
            let tree = Tree::new();
            let out = outside(&tree);
            symlink(&out, tree.root.join(FAKE).join("GBPUSD")).unwrap();
            let (format, _) = FakeFormat::new();
            let w =
                walk::walk_dataset(&tree.root, &format, "GBPUSD", 0, &WalkCaps::DEFAULT).unwrap();
            assert!(
                matches!(&w.dir, DatasetDir::Unreadable { why } if why.contains("symbolic link"))
            );
            assert!(w.daily.is_empty(), "the link was not followed");
            assert_eq!(w.skipped[0].class, EntryClass::Symlink);
            assert_eq!(w.skipped[0].path, format!("{FAKE}/GBPUSD"), "relative to the root");
        }

        #[test]
        fn a_symlinked_year_and_a_symlinked_file_are_skipped_and_never_read() {
            let tree = Tree::new();
            let out = outside(&tree);
            symlink(out.join("2"), tree.dataset().join("2")).unwrap();
            fs::create_dir_all(tree.dataset().join("3")).unwrap();
            symlink(
                out.join("2").join(format!("{}.day", MON / DAY)),
                tree.dataset().join("3").join(format!("{}.day", (MON + DAY) / DAY)),
            )
            .unwrap();
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            assert!(w.daily.is_empty(), "neither link was followed: {:?}", w.daily.keys());
            let classes: Vec<_> = w.skipped.iter().map(|s| s.class).collect();
            assert_eq!(classes, vec![EntryClass::Symlink, EntryClass::Symlink]);
        }

        #[test]
        fn a_hard_linked_file_is_skipped() {
            let tree = Tree::new();
            let out = outside(&tree);
            fs::create_dir_all(tree.dataset().join("2")).unwrap();
            fs::hard_link(
                out.join("2").join(format!("{}.day", MON / DAY)),
                tree.dataset().join("2").join(format!("{}.day", MON / DAY)),
            )
            .unwrap();
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            assert!(w.daily.is_empty());
            assert_eq!(w.skipped.len(), 1);
            assert_eq!(w.skipped[0].class, EntryClass::HardLinked);
        }

        fn mkfifo(path: &Path) {
            let status = Command::new("mkfifo").arg(path).status().expect("mkfifo runs");
            assert!(status.success(), "mkfifo {}", path.display());
        }

        /// A FIFO named like a day file is skipped by the walk — and one swapped in AFTER the walk
        /// is refused by the open WITHOUT parking the thread (`O_NONBLOCK`), as not a regular file.
        #[test]
        fn a_fifo_is_skipped_and_one_swapped_in_after_the_walk_never_parks_the_open() {
            let tree = Tree::new();
            fs::create_dir_all(tree.dataset().join("2")).unwrap();
            mkfifo(&tree.dataset().join("2").join(format!("{}.day", MON / DAY)));
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            assert!(w.daily.is_empty());
            assert_eq!(w.skipped[0].class, EntryClass::Fifo);

            // Swap: vet a real file, then replace it with a FIFO of the same name.
            let path = tree.daily(MON + DAY, b"\x05abc");
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            let file = w.daily[&(MON + DAY)].clone();
            fs::remove_file(&path).unwrap();
            mkfifo(&path);
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(walk::read_vetted(&file, 64));
            });
            let read = rx.recv_timeout(WAIT).expect("the open must not block on a FIFO");
            assert_eq!(read.expect_err("a FIFO is refused").class, walk::CHANGED_SINCE_WALK);
        }

        /// **THE SWAP.** A file replaced between the walk and the open — by a rename, so the name is
        /// the same, the LENGTH is the same and only the object differs — is refused, and the planted
        /// bytes are never returned. Only the `(dev, ino)` comparison can see this.
        #[test]
        fn a_file_swapped_between_the_walk_and_the_open_is_refused_and_not_read() {
            let tree = Tree::new();
            let path = tree.daily(MON, b"\x05real");
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            let file = &w.daily[&MON];

            let planted = tree._tmp.path().join("planted");
            fs::write(&planted, b"\x05EVIL").unwrap(); // the SAME length as the vetted file
            fs::rename(&planted, &path).unwrap();

            match walk::read_vetted(file, 64) {
                Err(refusal) => {
                    assert_eq!(refusal.class, walk::CHANGED_SINCE_WALK, "{refusal:?}");
                    assert!(!refusal.detail.contains("EVIL"), "no file byte is ever echoed");
                }
                Ok(bytes) => {
                    panic!("the swapped-in file was READ: {:?}", String::from_utf8_lossy(&bytes))
                }
            }
            assert!(
                walk::read_vetted_prefix(file, 1).is_err(),
                "the header read is held to it too"
            );

            // ...and a symlink swapped in for the file is refused at the open (O_NOFOLLOW).
            let w = walk_of(&tree, &WalkCaps::DEFAULT);
            let file = w.daily[&MON].clone();
            let out = outside(&tree);
            fs::remove_file(&path).unwrap();
            symlink(out.join("2").join(format!("{}.day", MON / DAY)), &path).unwrap();
            assert_eq!(
                walk::read_vetted(&file, 64).expect_err("a link").class,
                walk::CHANGED_SINCE_WALK
            );
        }
    }
}
