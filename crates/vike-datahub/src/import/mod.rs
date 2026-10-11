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
mod tests;
