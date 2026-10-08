//! **The ARCHIVE IMPORT verb's vocabulary** — the request, the answer, and the rules BOTH ends need
//! for [`Request::ImportArchive`](crate::proto::Request::ImportArchive) /
//! [`Response::ArchiveImported`](crate::proto::Response::ArchiveImported).
//!
//! The approved design is `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`. Its
//! shape in one paragraph: the user downloads a vendor archive under THEIR OWN account (Dukascopy's
//! Requester-Pays bucket, through `aws s3 sync`) into a folder on the DATAHUB's box, and this verb
//! reads one dataset of that folder into the served store. No vendor credential ever enters vike —
//! an archive only vike can convert is not a deliverable, so the user does the download and vike
//! does the import, with a plan shown first.
//!
//! # What the request names, and what it deliberately cannot
//!
//! - **A FORMAT** — a registry id the SERVER owns and advertises as one `import_format=<id>` entry
//!   per registered format ([`crate::proto::import_format_feature`]). The format decides the
//!   layout grammar, the time base, the price scale, the commit keys AND the venue, so the request
//!   names **no venue**: a Dukascopy archive can only land under `venue=dukascopy`.
//! - **ONE dataset directory NAME** — never a path. The server composes
//!   `<imports root>/<format>/<dataset>` itself, and [`validate_import_dataset`] is what makes that
//!   composition safe: this is the first verb on this wire whose argument names a filesystem object,
//!   however confined, so the name is a SECURITY boundary rather than hygiene.
//! - **An inclusive window of UTC DAYS**, each bound the epoch-ms START of a day, and the bar
//!   intervals to derive per imported day. Days are this verb's unit, which is why a bound is a day
//!   start rather than an instant.
//! - **`dry_run` / `verify`** — the plan is ALWAYS computed and returned; `dry_run` decides only
//!   whether the import half runs, and `verify` (only beside `dry_run`) decodes every planned file
//!   while still writing nothing.
//!
//! # What the answer carries, and what it never does
//!
//! [`ImportDone`] has [`crate::proto::DeleteDone`]'s shape — the one other verb on this wire that
//! answers "here is what I would do" and "here is what I did" with one type. The PLAN is the
//! SERVER's and is the authority: a client that previewed one and then executes gets a re-plan,
//! because the directory or the store may have moved since. It carries counts, day indices, commit
//! keys, the server-side directory and entry paths RELATIVE to the imports root — **never file
//! bytes**. A day's refusal names a class and a sentence, never content.
//!
//! # Why the rules live HERE
//!
//! The rule [`crate::seed`] and [`crate::catalog`] state, for their reason: a client cannot guard a
//! rule the server does not know, nor the reverse. [`validate_import_spec`] is called at BOTH doors
//! — [`crate::DatahubClient::import_archive`] before a frame is written, and the data daemon's own
//! handler before a directory is touched — so the refusal an operator reads locally is the refusal
//! the server would have given. The client copy is for the MESSAGE; the server copy is the
//! enforcement, and nothing on the client side substitutes for it. The bounds that are the
//! SERVER's alone — the per-file caps, the walk caps, the one-import slot — stay with the server,
//! because a client that knew them could only ever mis-predict them.
//!
//! # Scope, and why it is not Observe
//!
//! [`crate::proto::required_scope`] puts this verb in [`crate::proto::VerbScope::Write`] beside
//! `Backfill`, and its arm carries the argument. The short form: it WRITES the served store (rows
//! only `DeleteSeries` can remove), the CLIENT names the window — the fourth leg of
//! `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`'s decision 3 fails — it steers
//! server-side filesystem reads, and a mis-scaled day spends its commit key irreversibly. It
//! removes nothing, so it takes `Backfill`'s posture rather than `DeleteSeries`': served on a
//! key-less LOOPBACK datahub, Control-only on a keyed one.

use serde::{Deserialize, Serialize};
use vike_data::SeriesCoverage;
use vike_model::MS_PER_DAY;

/// The longest dataset name a request may carry.
///
/// The design's own bound (§2.3): a dataset is ONE vendor instrument folder, and the vendor's
/// folder names are short upper-case instrument codes (`EURUSD`). 32 leaves every plausible
/// instrument code room while bounding what a hostile frame can make the server compose into a
/// path before anything else runs.
pub const IMPORT_MAX_DATASET_BYTES: usize = 32;

/// The most UTC days ONE decoding request may cover — an import, or a `verify` dry run.
///
/// ⚠ **A bound on one SYNCHRONOUS request's duration, not on the total.** This wire has no
/// progress frame and a client's post-handshake read is unbounded by design
/// (`crates/vike-datahub-client/src/client.rs`'s `arm_request_timeouts`), so a request that
/// decoded twenty years in one go would hold a socket and a slot for as long as that took with
/// nobody able to tell "busy" from "dead". 31 days is one calendar month at most, so a client that
/// splits a long window by MONTH never meets it and a killed session resumes at the next month.
///
/// A plan-only dry run is NOT bounded by it: it walks and classifies and reads file headers, and
/// decodes nothing, which is what lets an operator see the whole dataset before importing any of
/// it. The server re-checks this cap after it resolves an omitted bound, which the client cannot.
pub const IMPORT_MAX_DAYS: i64 = 31;

/// The most bar intervals ONE request may ask to derive per imported day — bounds the resample work
/// each day costs (design §5).
pub const IMPORT_MAX_BAR_INTERVALS: usize = 4;

/// The longest spelling of ONE bar interval a request may carry.
///
/// ⚠ **This bound exists to protect the PARSER, not to classify intervals.**
/// `crates/vike-model/src/time/mod.rs`'s `interval_ms` multiplies a parsed `i64` count by the unit's
/// width, so a nineteen-digit count overflows that multiplication — a panic in a debug build. Every
/// interval that divides a UTC day is at most six bytes (`86400s`), so eight refuses nothing real
/// and keeps every count far below the overflow.
pub const IMPORT_MAX_INTERVAL_BYTES: usize = 8;

/// **The ONE dataset validator, shared by both ends** — the seed-and-catalog validator pattern
/// applied to the one argument on this wire that becomes a PATH COMPONENT.
///
/// The rules (design §2.3), cheapest first, each naming what was wrong:
///
/// 1. non-empty, and at most [`IMPORT_MAX_DATASET_BYTES`];
/// 2. the FIRST byte is `[A-Z0-9]` — which refuses `.`, `..`, every leading dot, an absolute path
///    and a separator in first position;
/// 3. EVERY byte is `[A-Z0-9._-]` — which refuses every separator (`/`, `\`), a drive letter's
///    `:`, NUL and every other control byte, whitespace, and lower case;
/// 4. not a Windows reserved DEVICE NAME (`CON`, `NUL`, `COM1`…) under any extension.
///
/// ⚠ **Upper case only, and that is a data rule as much as a safety one.** The vendor's folders are
/// upper-case, and the dataset becomes the series SYMBOL. Admitting `eurusd` beside `EURUSD` would
/// give one instrument two series — the defect the HTTP lane already has for a symbol typed in the
/// wrong case — and the same files imported twice under two names.
///
/// ⚠ **Rule 4 is DELEGATED, not restated.** After rules 1–3 pass, the name is also handed to
/// `crates/vike-model/src/runs.rs`'s `valid_mark_name`, which owns this workspace's one list of
/// device names and its one stem rule (`CON` and `CON.X` alike). Rules 1–3 are strictly narrower
/// than everything else that function checks — its charset, its leading-dot rule, its depth and
/// whitespace rules — so a name that reaches it can only fail it on the device-name rule, and
/// `a_name_that_passes_the_charset_fails_the_mark_rule_only_as_a_device_name` holds that claim
/// rather than trusting it.
///
/// ⚠ **The message never ECHOES the dataset**, the rule `crate::seed::validate_seed_symbol` and
/// [`crate::catalog::validate_catalog_venue`] state: the field being refused is the untrusted one, a
/// refusal quoting it is that same untrusted string wearing a server log line, and the file layer
/// defaults to `trace`. It names the LENGTH, the CAP and the OFFSET, which is what an operator can
/// act on.
///
/// ⚠ **A dataset is NOT trimmed and NOT upper-cased here.** The server composes the path from the
/// name it was sent, so coercing it would make the request and the directory read disagree about
/// which folder was asked for.
pub fn validate_import_dataset(dataset: &str) -> Result<(), String> {
    if dataset.is_empty() {
        return Err("an import dataset is EMPTY, so it names no folder. Pass the vendor's own \
                    upper-case instrument folder name (`EURUSD`)."
            .to_string());
    }
    if dataset.len() > IMPORT_MAX_DATASET_BYTES {
        return Err(format!(
            "an import dataset of {} bytes exceeds IMPORT_MAX_DATASET_BYTES = \
             {IMPORT_MAX_DATASET_BYTES}. A dataset is ONE instrument folder name, never a path.",
            dataset.len()
        ));
    }
    let first = dataset.as_bytes()[0];
    if !(first.is_ascii_uppercase() || first.is_ascii_digit()) {
        return Err(
            "an import dataset must START with an upper-case ASCII letter or a digit. That refuses \
             `.`, `..`, a leading dot and an absolute path outright: the name becomes ONE path \
             component under the server's imports directory, and nothing it says may climb out of \
             it or hide from a directory listing."
                .to_string(),
        );
    }
    if let Some(at) = dataset.bytes().position(|b| !is_dataset_byte(b)) {
        return Err(format!(
            "an import dataset carries a byte outside the permitted set at offset {at}. A dataset \
             may hold upper-case ASCII letters, digits, `.`, `_` and `-` only — no separator, no \
             drive letter, no control byte and no lower case, because it is composed into a path \
             on the datahub's box and becomes the series symbol. The vendor's own folder names are \
             upper case."
        ));
    }
    if vike_model::runs::valid_mark_name(dataset).is_err() {
        return Err(
            "an import dataset is a reserved Windows DEVICE NAME (`CON`, `PRN`, `AUX`, `NUL`, \
             `COM1`–`COM9`, `LPT1`–`LPT9`, under any extension). A folder called that cannot be \
             opened on Windows, so no dataset may be named after one."
                .to_string(),
        );
    }
    Ok(())
}

/// The bytes a dataset may be built from: upper-case ASCII letters, digits, `.`, `_` and `-`.
///
/// An ALLOWLIST, for the reason every validator in this crate is one: the set of dangerous bytes
/// in a path component is open — separators, `:`, NUL, a newline a log line would carry — and the
/// set of safe ones is one line.
fn is_dataset_byte(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
}

/// The bar-interval rules: at most [`IMPORT_MAX_BAR_INTERVALS`] of them, no repeat, and each one a
/// CANONICAL spelling of a step that DIVIDES a UTC day.
///
/// An EMPTY list is valid and means "derive no bars" — the CLI's `--bars none`.
///
/// Why each rule:
///
/// - **It must divide a UTC day** (design §3.5). Bars are derived PER IMPORTED DAY, and an
///   interval that does not divide one (`7m`) has buckets that straddle midnight, which a per-day
///   resample cannot build. Those bars are derived later from the stored ticks instead.
/// - **Canonical spelling — no leading zero, no zero count.** The interval string becomes the
///   bar series' INTERVAL partition, so `01m` beside `1m` would be two series of identical bars,
///   and `0m` measures (as zero) but is a bucket of no width.
/// - **At most [`IMPORT_MAX_INTERVAL_BYTES`] before it is parsed**, which protects the parser — see
///   that constant.
/// - **No repeat**, because a repeated interval asks for nothing the first mention did not and
///   would let a request pad past [`IMPORT_MAX_BAR_INTERVALS`] with work the cap exists to bound.
pub fn validate_import_bars(bars: &[String]) -> Result<(), String> {
    if bars.len() > IMPORT_MAX_BAR_INTERVALS {
        return Err(format!(
            "an import asks for {} bar intervals, over IMPORT_MAX_BAR_INTERVALS = \
             {IMPORT_MAX_BAR_INTERVALS} — the resample work every imported day costs. Ask for \
             fewer, and derive the rest later from the stored ticks.",
            bars.len()
        ));
    }
    for (i, iv) in bars.iter().enumerate() {
        if iv.len() > IMPORT_MAX_INTERVAL_BYTES {
            return Err(format!(
                "bar interval {i} is {} bytes, over IMPORT_MAX_INTERVAL_BYTES = \
                 {IMPORT_MAX_INTERVAL_BYTES}. Every interval that divides a UTC day is at most six \
                 (`86400s`).",
                iv.len()
            ));
        }
        let width = vike_model::time::interval_ms(iv);
        let canonical = iv.as_bytes().first().is_some_and(|&b| matches!(b, b'1'..=b'9'));
        match width {
            Some(w) if w > 0 && canonical => {
                if MS_PER_DAY % w != 0 {
                    return Err(format!(
                        "bar interval {iv:?} does not divide a UTC day, so its buckets straddle \
                         midnight and a per-day import cannot build them. Import with a step that \
                         divides a day (`1m`, `5m`, `1h`, `1d`), and derive {iv:?} from the stored \
                         ticks afterwards."
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "bar interval {iv:?} is not a canonical bar step: a count with no leading zero \
                     followed by one of `s`/`m`/`h`/`d` (`1m`, `15m`, `4h`, `1d`). The string \
                     becomes the bar series' interval, so a second spelling of one width would be a \
                     second series."
                ));
            }
        }
        if bars[..i].contains(iv) {
            return Err(format!(
                "bar interval {iv:?} is named twice. A repeated interval asks for nothing the first \
                 mention did not; name each once."
            ));
        }
    }
    Ok(())
}

/// The WINDOW rules: each bound present is the START of a UTC day, the bounds are in order, `verify`
/// rides only on a dry run, and a request that DECODES spans at most [`IMPORT_MAX_DAYS`] days when
/// both of its bounds are known.
///
/// ⚠ **An omitted bound is not refused and the day cap is not checked against it here.** `None`
/// means "the dataset's first (or last) day", which only the server can resolve, so it re-checks
/// the cap after resolving — this function says what can be said without the directory.
///
/// ⚠ **`verify` without `dry_run` is REFUSED rather than read one way or the other.** `verify`
/// means "decode every planned file and still write nothing"; beside an execute it would either be
/// ignored (so the operator believes a verification ran) or turn the execute into a dry run (so the
/// operator believes an import ran). Every input whose meaning is uncertain is refused before a key
/// is spent — the design's posture for this whole lane.
pub fn validate_import_window(
    from_day: Option<i64>,
    to_day: Option<i64>,
    dry_run: bool,
    verify: bool,
) -> Result<(), String> {
    if verify && !dry_run {
        return Err("`verify` is a mode of a DRY RUN — it decodes every planned file and writes \
                    nothing — and this request is an import. Send `dry_run` with it, or drop \
                    `verify`; nothing was read and nothing was written."
            .to_string());
    }
    for (name, bound) in [("from_day", from_day), ("to_day", to_day)] {
        if let Some(ms) = bound
            && ms.rem_euclid(MS_PER_DAY) != 0
        {
            return Err(format!(
                "`{name}` = {ms} is not the START of a UTC day. A bound is an inclusive DAY, sent \
                 as the epoch-ms of that day's UTC midnight — days are this verb's unit, unlike \
                 `data hist fetch`, whose window end is an instant."
            ));
        }
    }
    if let (Some(from), Some(to)) = (from_day, to_day) {
        if from > to {
            return Err(format!(
                "the import window is inverted — from_day {from} is after to_day {to}. Both bounds \
                 are inclusive UTC days."
            ));
        }
        // Saturating: both bounds are aligned but otherwise any `i64` a frame can carry, and a
        // plain subtraction of two far-apart ones would overflow — a panic in a debug build, on a
        // server thread, from a request. Saturated, the span is simply huge and refused below.
        let days = to.saturating_sub(from) / MS_PER_DAY + 1;
        if (verify || !dry_run) && days > IMPORT_MAX_DAYS {
            return Err(format!(
                "this request would decode {days} days, over IMPORT_MAX_DAYS = {IMPORT_MAX_DAYS}: \
                 one request is synchronous and this wire carries no progress frame, so a long \
                 decode is indistinguishable from a dead server. Split the window by calendar month \
                 (a plan-only dry run is not bounded by this cap)."
            ));
        }
    }
    Ok(())
}

/// **Every client-checkable rule on one request, in one place** — the dataset, the bars and the
/// window, cheapest first. Called at BOTH doors (see this module's doc).
///
/// ⚠ The FORMAT is deliberately not validated here: it is matched by exact equality against the
/// server's own registry (and, on the client, against the server's own advertisement), so a string
/// that is not a registered id is refused BY that lookup, naming what IS registered.
pub fn validate_import_spec(spec: &ImportSpec) -> Result<(), String> {
    validate_import_dataset(&spec.dataset)?;
    validate_import_bars(&spec.bars)?;
    validate_import_window(spec.from_day, spec.to_day, spec.dry_run, spec.verify)
}

/// One [`Request::ImportArchive`](crate::proto::Request::ImportArchive): import ONE dataset of one
/// archive FORMAT from the SERVER's own imports directory into the store.
///
/// ⚠ **The design sketches this as a struct VARIANT and it rides as a newtype variant over this
/// struct, which is the SAME BYTES on the wire.** serde encodes `ImportArchive(ImportSpec)` as
/// `{"ImportArchive":{"format":…,"dataset":…,…}}`, exactly the externally-tagged encoding of a
/// struct variant with these fields; `the_import_frame_has_the_designed_shape` pins the keys.
/// The struct exists so ONE value can be validated, sent and logged without a seven-argument
/// signature at every hop.
///
/// ⚠ **`dry_run` has NO serde default, deliberately.** A frame that omits it fails to DECODE and is
/// answered `Response::Error`; with a default it would decode as `false` — an IMPORT — so the one
/// field that decides whether the store is written could be dropped by accident and the request
/// would write. `a_frame_that_omits_dry_run_does_not_decode` holds that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSpec {
    /// A registry id the server advertised as `import_format=<id>` — `"dukascopy-bi5"`. It is also
    /// the directory name under the imports root, and it decides the venue.
    pub format: String,
    /// ONE path segment under `<imports root>/<format>/` — `"EURUSD"`. See
    /// [`validate_import_dataset`]. It is also the series SYMBOL the rows land under.
    pub dataset: String,
    /// Inclusive first day: the epoch-ms START of a UTC day. `None` = the dataset's first day.
    pub from_day: Option<i64>,
    /// Inclusive last day: the epoch-ms START of a UTC day. `None` = the dataset's last day.
    pub to_day: Option<i64>,
    /// Bar intervals to derive per imported day, each dividing a UTC day. Empty = derive none. See
    /// [`validate_import_bars`].
    pub bars: Vec<String>,
    /// Plan only: walk, classify, read file headers; decode nothing, write nothing.
    pub dry_run: bool,
    /// With `dry_run`: DECODE every planned file too, still writing nothing. Refused without it.
    pub verify: bool,
}

impl ImportSpec {
    /// Whether this request DECODES files — an import, or a `verify` dry run. These are the
    /// requests [`IMPORT_MAX_DAYS`] bounds.
    pub fn decodes(&self) -> bool {
        self.verify || !self.dry_run
    }

    /// Whether this request may WRITE the store — exactly the requests that are not dry runs.
    pub fn writes(&self) -> bool {
        !self.dry_run
    }
}

/// The [`Response::ArchiveImported`](crate::proto::Response::ArchiveImported) payload: what one
/// import planned and — unless it was a plan-only dry run — what it then did.
///
/// The PLAN is always present. The OUTCOME is `None` iff the request was a plan-only dry run
/// (`dry_run` without `verify`); a `verify` dry run returns an outcome whose days are
/// [`DayResult::Verified`] or [`DayResult::Refused`] and wrote nothing.
///
/// ⚠ A WHOLE-REQUEST refusal is not this shape at all — an unknown format, an invalid dataset, an
/// instrument the format cannot scale, a window over the day cap, a second concurrent import. Those
/// are `Response::Error`, because nothing was planned. A refused DAY is a value inside the plan or
/// the outcome, because the request carried on past it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDone {
    /// What the server found and decided, day by day. The SERVER's plan is the authority.
    pub plan: ImportPlan,
    /// What it did. `None` iff the request was a plan-only dry run.
    pub outcome: Option<ImportOutcome>,
}

/// Whether the dataset's directory could be walked at all.
///
/// Three states rather than a `bool`, because "absent" and "exists, cannot be read" call for
/// different fixes: the first is a sync that has not happened (or happened on the wrong box — the
/// files must be on the DATAHUB's box), the second a permission or a sandbox, such as a unit whose
/// `ProtectHome` hides a root symlinked into a home directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatasetDir {
    /// The directory exists and was walked.
    Present,
    /// No such directory on the server's box. The rest of the plan is empty, and a client renders
    /// [`ImportPlan::server_dir`] with the ways to fill it.
    Absent,
    /// The directory exists and could not be read. `why` is the server's sentence — a class of
    /// error, never file content.
    Unreadable {
        /// Why the walk could not proceed.
        why: String,
    },
}

/// The server's PLAN for one import — always computed, always returned.
///
/// Every day is a [`DayPlan`] carrying its classification; every field below is a COUNT, a day
/// index (epoch-ms of a UTC midnight), a commit key, or a path on the SERVER's box. No file byte
/// crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlan {
    /// The format id, echoed — the registry's own spelling.
    pub format: String,
    /// The dataset, echoed.
    pub dataset: String,
    /// The store venue every row of this format lands under — decided by the format, never by the
    /// request.
    pub venue: String,
    /// The dataset directory ON THE DATAHUB'S BOX, as the server resolved it. It is the plan's first
    /// line for a reason: a tunnelled datahub is `127.0.0.1` too, so this is how an operator learns
    /// WHICH box the files must be on.
    pub server_dir: String,
    /// Whether that directory could be walked.
    pub dir: DatasetDir,
    /// The format's own one-line statement of why this dataset is admitted — for Dukascopy's daily
    /// files, the instrument's point value and the evidence for it. A dataset the format cannot
    /// admit never gets a plan: that is a whole-request refusal.
    pub admission: String,
    /// What the walk found in the whole dataset, whatever the window.
    pub inventory: ArchiveInventory,
    /// The RESOLVED inclusive first day — the request's bound, or the dataset's first daily-file day
    /// when it omitted one. `None` only when it omitted one and the dataset holds no daily file.
    pub from_day: Option<i64>,
    /// The RESOLVED inclusive last day, by the same rule.
    pub to_day: Option<i64>,
    /// Every day inside the resolved window that HAS a daily file, ascending, each classified. A day
    /// with no file is never here — see [`Self::gaps`].
    pub days: Vec<DayPlan>,
    /// Weekdays inside both the resolved window and the dataset's own span that hold NO daily file,
    /// ascending. Per the vendor a missing file means no ticks were recorded — or the sync has not
    /// fetched it yet; a gap spends nothing, so a later sync fills it.
    pub gaps: Vec<i64>,
    /// The intervals that will be derived per imported day, echoed from the request.
    pub bars: Vec<String>,
    /// The quote series' coverage as the store holds it NOW, from either lane, or `None` when it
    /// holds nothing. A client extrapolates the store growth from its `bytes` / `rows`; with `None`
    /// the growth is unknown until the first month lands, and a plan must say so rather than guess.
    pub series: Option<SeriesCoverage>,
}

impl ImportPlan {
    /// How many days the import half would store — [`DayClass::Free`] plus
    /// [`DayClass::Supersede`]. The number a typed confirmation names.
    pub fn importable_days(&self) -> usize {
        self.days.iter().filter(|d| d.class.is_importable()).count()
    }
}

/// What the walk found in the dataset, over its WHOLE span.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveInventory {
    /// Daily files of the importable layout.
    pub daily_files: u64,
    /// The first and last day holding a daily file, `None` when there is none.
    pub first_day: Option<i64>,
    /// See [`Self::first_day`].
    pub last_day: Option<i64>,
    /// Days that hold files of a RECOGNISED layout this format does not import (for Dukascopy, the
    /// hourly layout) and no daily file, ascending. Counted and reported, never imported. A day
    /// holding BOTH layouts is not here: it is a refused [`DayPlan`].
    pub other_layout_days: Vec<i64>,
    /// Entries with an unexpected name or at an unexpected depth — counted with their bytes and
    /// never opened.
    pub other_objects: u64,
    /// The bytes of [`Self::other_objects`].
    pub other_bytes: u64,
    /// Entries the walk refused to follow or open — see [`EntryClass`]. Each is named RELATIVE to
    /// the imports root. Bounded by the server's own walk cap.
    pub skipped: Vec<SkippedEntry>,
}

/// One entry the walk classified and SKIPPED, never opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedEntry {
    /// The entry's path RELATIVE to the imports root — never an absolute path, never content.
    pub path: String,
    /// Why it was skipped.
    pub class: EntryClass,
}

/// Why the walk skipped an entry it found where it expected a directory or a file.
///
/// The walk never FOLLOWS anything: every entry is examined without resolving it, and only plain
/// directories and plain files with the expected names are accepted. Each variant here is a planted
/// object the checks turned into a refused entry rather than a read outside the root or a hung
/// thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryClass {
    /// A symbolic link — never followed.
    Symlink,
    /// A named pipe — never opened, because an open on one can block.
    Fifo,
    /// A socket.
    Socket,
    /// A block or character device.
    Device,
    /// A regular file with more than one hard link (unix), which could be a second name for a file
    /// outside the root.
    HardLinked,
    /// Any other kind of filesystem object.
    Unknown,
}

/// One day of the plan: the day, its classification, and what its file declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayPlan {
    /// The epoch-ms START of the UTC day.
    pub day: i64,
    /// What the import does with it.
    pub class: DayClass,
    /// The daily file's size on disk.
    pub file_bytes: u64,
    /// The tick count the file's own header DECLARES, when it declares a size — read from the
    /// header without decoding. `None` when the file does not declare one.
    pub declared_ticks: Option<u64>,
}

/// What the import does with one day that HAS a file — the design's one-owner-per-day table (§4.2),
/// checked in this order, the first that applies deciding.
///
/// ⚠ The order is load-bearing and it is the variant order: a held day is never also reported as
/// overlapped, and a recent day is never imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayClass {
    /// The day ends inside the vendor's publication margin. Not imported: "import again later".
    TooRecent,
    /// The archive lane already stored this day under its own day key. The file is skipped; any
    /// requested interval still missing is derived from the stored ticks, with no decode.
    HeldByArchive,
    /// The HTTP lane already stored this day under ITS canonical day key. Skipped and topped up,
    /// exactly as [`Self::HeldByArchive`].
    HeldByHttp,
    /// The only key meeting the day is a PROVISIONAL one whose range IS the day. The import stores
    /// the day and supersedes `key`.
    Supersede {
        /// The provisional commit key the import supersedes.
        key: String,
    },
    /// Some other key of the venue's quote lanes meets the day — a ragged edge, a tail, a provisional
    /// window with other bounds. REFUSED, because importing would store the overlapped ticks twice;
    /// the day stays the other lane's to fill.
    Overlapped {
        /// The commit keys that meet the day.
        keys: Vec<String>,
    },
    /// No key meets the day. The import decodes it, stores it under the archive day key and derives
    /// the requested bars.
    Free,
    /// The day was refused at plan time — a mixed-layout day, or a file whose header already breaks
    /// a cap.
    Refused(DayRefusal),
}

impl DayClass {
    /// Whether the import half STORES this day — [`Self::Free`] and [`Self::Supersede`].
    pub fn is_importable(&self) -> bool {
        matches!(self, Self::Free | Self::Supersede { .. })
    }
}

/// Why ONE day was refused — a stable CLASS token and an operator's sentence.
///
/// ⚠ **A string class rather than an enum, and that is the layering rather than laziness.** The
/// classes are each FORMAT's own (a Dukascopy daily file is refused as `AmbiguousTimeBase` or
/// `NotMonotonic`; another vendor's archive will have its own), and this crate sits below every
/// format and names none of them. An enum here would have to grow a variant per vendor's decoder,
/// and a new decoder refusal would become a wire change. `class` is a CamelCase token a client may
/// match on and a human can grep for; `detail` says what to do. Neither ever carries file bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRefusal {
    /// The refusal's stable class token — `AmbiguousTimeBase`, `NotMonotonic`, `MixedLayout`…
    pub class: String,
    /// An operator's sentence: what was wrong, and that no commit key was spent.
    pub detail: String,
}

/// What the import half did — present on an import and on a `verify` dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// One row per day the import half acted on, ascending.
    pub days: Vec<DayOutcome>,
}

/// One day's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayOutcome {
    /// The epoch-ms START of the UTC day.
    pub day: i64,
    /// What happened to it.
    pub result: DayResult,
}

/// What happened to one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayResult {
    /// Decoded and stored: `ticks` quotes under the archive day key, and the bars derived from them.
    Imported {
        /// Ticks stored.
        ticks: u64,
        /// Bars written per requested interval.
        bars: Vec<BarsWritten>,
    },
    /// A HELD day: nothing decoded, and the requested intervals that were missing derived from the
    /// stored ticks. An empty list means none was missing.
    ToppedUp {
        /// Bars written per interval that was missing.
        bars: Vec<BarsWritten>,
    },
    /// A `verify` dry run decoded the file and it passed every check. Nothing was written.
    Verified {
        /// Ticks the file decoded to.
        ticks: u64,
    },
    /// Refused while decoding or storing. No commit key was spent for it, and the request carried on
    /// to the next day.
    Refused(DayRefusal),
}

/// How many bars ONE interval wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarsWritten {
    /// The interval, as the request named it.
    pub interval: String,
    /// Bars written (0 when every bucket was already stored).
    pub rows: u64,
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::proto::{Request, Response, read_frame, write_frame};

    const DAY: i64 = MS_PER_DAY;
    /// 2024-01-15 00:00 UTC — the day the design's measured file belongs to.
    const D0: i64 = 19_737 * DAY;

    fn spec() -> ImportSpec {
        ImportSpec {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: Some(D0),
            to_day: Some(D0 + 30 * DAY),
            bars: vec!["1m".to_string()],
            dry_run: false,
            verify: false,
        }
    }

    fn refused(dataset: &str) -> String {
        validate_import_dataset(dataset).expect_err("must be refused")
    }

    // ---- the dataset validator: one test per refusal the design's §8 names ----------------------------

    #[test]
    fn the_vendor_folder_names_are_accepted() {
        for good in [
            "EURUSD", "USDJPY", "XAUUSD", "E", "1", "US30.IDX", "BTC-USD", "EUR_USD",
            // A device-name PREFIX is not a device name: only the exact stem is reserved.
            "CONX", "COM10", "LPT0", "NULL",
        ] {
            validate_import_dataset(good)
                .unwrap_or_else(|e| panic!("{good} must be accepted: {e}"));
        }
    }

    #[test]
    fn dot_dot_is_refused() {
        assert!(refused("..").contains("must START"), "{}", refused(".."));
    }

    #[test]
    fn a_separator_is_refused() {
        for bad in ["EUR/USD", "EUR\\USD", "EURUSD/"] {
            assert!(refused(bad).contains("outside the permitted set"), "{bad}: {}", refused(bad));
        }
    }

    #[test]
    fn an_absolute_path_is_refused() {
        for bad in ["/ETC", "\\\\SERVER\\SHARE"] {
            assert!(refused(bad).contains("must START"), "{bad}: {}", refused(bad));
        }
    }

    #[test]
    fn a_drive_letter_is_refused() {
        for bad in ["C:", "C:EURUSD", "C:\\EURUSD"] {
            assert!(refused(bad).contains("offset 1"), "{bad}: {}", refused(bad));
        }
    }

    #[test]
    fn a_leading_dot_is_refused() {
        for bad in [".", ".EURUSD", ".HIDDEN"] {
            assert!(refused(bad).contains("must START"), "{bad}: {}", refused(bad));
        }
    }

    #[test]
    fn a_nul_is_refused() {
        let err = refused("EUR\0USD");
        assert!(err.contains("offset 3"), "{err}");
        assert!(refused("\0").contains("must START"));
    }

    #[test]
    fn thirty_three_bytes_are_refused_and_thirty_two_are_not() {
        let at_cap = "A".repeat(IMPORT_MAX_DATASET_BYTES);
        validate_import_dataset(&at_cap).expect("exactly at the cap is accepted");
        let over = "A".repeat(IMPORT_MAX_DATASET_BYTES + 1);
        let err = refused(&over);
        assert!(err.contains("IMPORT_MAX_DATASET_BYTES"), "{err}");
        assert_eq!(over.len(), 33);
    }

    #[test]
    fn lower_case_is_refused() {
        assert!(refused("eurusd").contains("must START"));
        assert!(refused("EURusd").contains("offset 3"), "{}", refused("EURusd"));
    }

    #[test]
    fn a_windows_device_name_is_refused() {
        for bad in
            ["CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9", "CON.TXT", "NUL.EURUSD"]
        {
            assert!(refused(bad).contains("DEVICE NAME"), "{bad}: {}", refused(bad));
        }
    }

    #[test]
    fn an_empty_dataset_is_refused() {
        assert!(refused("").contains("EMPTY"));
    }

    #[test]
    fn whitespace_is_refused() {
        for bad in ["EUR USD", "EURUSD\n", " EURUSD"] {
            let _ = refused(bad);
        }
    }

    /// The validator never ECHOES what it refused — the untrusted string would otherwise ride into the
    /// server's log. Each input carries a token that appears in no refusal text.
    #[test]
    fn no_refusal_echoes_the_dataset() {
        let long = "Q".repeat(IMPORT_MAX_DATASET_BYTES + 5);
        for bad in [
            "EURusdSECRET",
            "/ETCPASSWD",
            "C:WINDOWSX",
            "COM1.PAYLOADX",
            "..PAYLOADX",
            long.as_str(),
        ] {
            let err = refused(bad);
            assert!(!err.contains(bad), "the refusal echoed {bad:?}: {err}");
        }
    }

    /// ⚠ THE DELEGATION CLAIM `validate_import_dataset`'s doc makes, held rather than trusted: once a
    /// name passes rules 1–3, `vike_model::runs::valid_mark_name` can only refuse it as a DEVICE NAME.
    /// If that function ever grew a rule the dataset charset does not already imply — a shorter length
    /// cap, a banned character — a valid dataset would be refused here under a device-name message, and
    /// this test is what says so.
    ///
    /// Exhaustive over every name of up to three bytes drawn from an alphabet that covers both letters
    /// device names are built from, the digits, and the three separators, plus the 32-byte edge.
    #[test]
    fn a_name_that_passes_the_charset_fails_the_mark_rule_only_as_a_device_name() {
        let alphabet: Vec<char> = "ACLMNOPRTUX0129._-".chars().collect();
        let mut names: Vec<String> = Vec::new();
        for a in &alphabet {
            names.push(a.to_string());
            for b in &alphabet {
                names.push(format!("{a}{b}"));
                for c in &alphabet {
                    names.push(format!("{a}{b}{c}"));
                }
            }
        }
        for dev in ["CON", "PRN", "AUX", "NUL", "COM1", "LPT9"] {
            names.push(format!("{dev}.X"));
            names.push(format!("{dev}.TXT.GZ"));
        }
        names.push("A".repeat(IMPORT_MAX_DATASET_BYTES));
        names.push(format!("{}.{}", "B".repeat(15), "C".repeat(16)));
        let mut checked = 0usize;
        for n in &names {
            let passes_charset = !n.is_empty()
                && n.len() <= IMPORT_MAX_DATASET_BYTES
                && (n.as_bytes()[0].is_ascii_uppercase() || n.as_bytes()[0].is_ascii_digit())
                && n.bytes().all(is_dataset_byte);
            if !passes_charset {
                continue;
            }
            checked += 1;
            if let Err(why) = vike_model::runs::valid_mark_name(n) {
                assert!(
                    why.contains("device name"),
                    "valid_mark_name refused {n:?} for a reason that is not a device name — the \
                     dataset validator's delegation would now mislabel it: {why}"
                );
            }
        }
        assert!(checked > 1_000, "the sweep must not be vacuous: {checked}");
    }

    // ---- the bar intervals ----------------------------------------------------------------------------

    fn bars(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn day_dividing_canonical_intervals_are_accepted() {
        validate_import_bars(&[]).expect("no bars is `--bars none`");
        validate_import_bars(&bars(&["1m", "5m", "1h", "1d"])).expect("four day-dividing steps");
        validate_import_bars(&bars(&["86400s"])).expect("the longest day-dividing spelling");
        validate_import_bars(&bars(&["1440m"])).expect("a day, in minutes, divides a day");
    }

    #[test]
    fn more_than_the_interval_cap_is_refused() {
        let err = validate_import_bars(&bars(&["1m", "5m", "15m", "1h", "1d"])).unwrap_err();
        assert!(err.contains("IMPORT_MAX_BAR_INTERVALS"), "{err}");
    }

    #[test]
    fn an_interval_that_does_not_divide_a_day_is_refused() {
        for bad in ["7m", "13m", "5h", "7s"] {
            let err = validate_import_bars(&bars(&[bad])).unwrap_err();
            assert!(err.contains("does not divide a UTC day"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_non_canonical_or_unparseable_interval_is_refused() {
        for bad in ["01m", "0m", "1M", "1w", "1mo", "m", "", "1 m", "-1m", "+1m"] {
            let err = validate_import_bars(&bars(&[bad])).unwrap_err();
            assert!(err.contains("not a canonical bar step"), "{bad:?}: {err}");
        }
    }

    /// The length bound protects the PARSER: a nineteen-digit count would overflow `interval_ms`'s
    /// multiplication. Refused by length, before it is parsed, so no build panics on it.
    #[test]
    fn an_over_long_interval_is_refused_before_it_is_parsed() {
        let err = validate_import_bars(&bars(&["9223372036854775807d"])).unwrap_err();
        assert!(err.contains("IMPORT_MAX_INTERVAL_BYTES"), "{err}");
        assert!(!err.contains("9223372036854775807"), "the over-long string is not echoed: {err}");
    }

    #[test]
    fn a_repeated_interval_is_refused() {
        let err = validate_import_bars(&bars(&["1m", "5m", "1m"])).unwrap_err();
        assert!(err.contains("named twice"), "{err}");
    }

    // ---- the window --------------------------------------------------------------------------------

    #[test]
    fn an_open_window_is_accepted_in_every_mode() {
        for (dry_run, verify) in [(true, false), (true, true), (false, false)] {
            validate_import_window(None, None, dry_run, verify)
                .expect("open bounds are the server's");
        }
    }

    #[test]
    fn a_bound_that_is_not_a_day_start_is_refused() {
        let err = validate_import_window(Some(D0 + 1), None, true, false).unwrap_err();
        assert!(err.contains("`from_day`"), "{err}");
        let err = validate_import_window(None, Some(D0 + DAY - 1), true, false).unwrap_err();
        assert!(err.contains("`to_day`"), "{err}");
    }

    #[test]
    fn an_inverted_window_is_refused() {
        let err = validate_import_window(Some(D0 + DAY), Some(D0), true, false).unwrap_err();
        assert!(err.contains("inverted"), "{err}");
    }

    #[test]
    fn verify_without_dry_run_is_refused() {
        let err = validate_import_window(Some(D0), Some(D0), false, true).unwrap_err();
        assert!(err.contains("DRY RUN"), "{err}");
    }

    #[test]
    fn a_decoding_request_over_the_day_cap_is_refused_and_one_at_it_is_not() {
        let last = D0 + (IMPORT_MAX_DAYS - 1) * DAY;
        validate_import_window(Some(D0), Some(last), false, false).expect("31 days import");
        validate_import_window(Some(D0), Some(last), true, true).expect("31 days verify");
        for (dry_run, verify) in [(false, false), (true, true)] {
            let err =
                validate_import_window(Some(D0), Some(last + DAY), dry_run, verify).unwrap_err();
            assert!(err.contains("IMPORT_MAX_DAYS"), "{err}");
            assert!(err.contains("32 days"), "{err}");
        }
    }

    #[test]
    fn a_plan_only_dry_run_is_not_bounded_by_the_day_cap() {
        validate_import_window(Some(D0), Some(D0 + 3_650 * DAY), true, false)
            .expect("a plan reads headers and decodes nothing");
    }

    /// Two aligned bounds as far apart as an `i64` allows: a plain subtraction would overflow on a
    /// server thread. Refused, never a panic.
    #[test]
    fn extreme_bounds_do_not_overflow() {
        let lo = (i64::MIN / DAY) * DAY;
        let hi = (i64::MAX / DAY) * DAY;
        let err = validate_import_window(Some(lo), Some(hi), false, false).unwrap_err();
        assert!(err.contains("IMPORT_MAX_DAYS"), "{err}");
        validate_import_window(Some(lo), Some(hi), true, false).expect("a plan may span anything");
    }

    // ---- the request as a whole --------------------------------------------------------------------

    #[test]
    fn the_spec_validator_runs_every_rule() {
        validate_import_spec(&spec()).expect("the canonical month import is valid");
        let mut s = spec();
        s.dataset = "../ETC".to_string();
        assert!(validate_import_spec(&s).unwrap_err().contains("must START"));
        let mut s = spec();
        s.bars = bars(&["7m"]);
        assert!(validate_import_spec(&s).unwrap_err().contains("does not divide"));
        let mut s = spec();
        s.to_day = Some(D0 + 31 * DAY);
        assert!(validate_import_spec(&s).unwrap_err().contains("IMPORT_MAX_DAYS"));
    }

    #[test]
    fn decodes_and_writes_follow_the_two_flags() {
        let mut s = spec();
        assert!(s.decodes() && s.writes(), "an import decodes and writes");
        s.dry_run = true;
        assert!(!s.decodes() && !s.writes(), "a plan does neither");
        s.verify = true;
        assert!(s.decodes() && !s.writes(), "a verify decodes and writes nothing");
    }

    // ---- the wire shape ----------------------------------------------------------------------------

    /// The design sketches `Request::ImportArchive { format, dataset, from_day, to_day, bars, dry_run,
    /// verify }` as a STRUCT variant; it rides as a newtype variant over [`ImportSpec`]. The bytes are
    /// the same, and this pins them: one outer tag, exactly these seven keys.
    #[test]
    fn the_import_frame_has_the_designed_shape() {
        let v = serde_json::to_value(Request::ImportArchive(spec())).expect("encode");
        let outer = v.as_object().expect("an externally-tagged object");
        assert_eq!(outer.keys().map(String::as_str).collect::<Vec<_>>(), vec!["ImportArchive"]);
        let inner = outer["ImportArchive"].as_object().expect("the payload is an object");
        let mut keys: Vec<&str> = inner.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["bars", "dataset", "dry_run", "format", "from_day", "to_day", "verify"],
            "{v}"
        );
    }

    /// ⚠ A frame that OMITS `dry_run` does not decode — so the field that decides whether the store is
    /// written cannot be dropped by accident and default to an import.
    #[test]
    fn a_frame_that_omits_dry_run_does_not_decode() {
        let body = r#"{"ImportArchive":{"format":"dukascopy-bi5","dataset":"EURUSD","from_day":null,"to_day":null,"bars":[],"verify":false}}"#;
        assert!(
            serde_json::from_str::<Request>(body).is_err(),
            "an omitted dry_run must not decode"
        );
        let with = r#"{"ImportArchive":{"format":"dukascopy-bi5","dataset":"EURUSD","from_day":null,"to_day":null,"bars":[],"dry_run":true,"verify":false}}"#;
        match serde_json::from_str::<Request>(with).expect("the same frame WITH dry_run decodes") {
            Request::ImportArchive(s) => assert!(s.dry_run),
            other => panic!("expected ImportArchive, got {other:?}"),
        }
    }

    fn populated_done() -> ImportDone {
        ImportDone {
            plan: ImportPlan {
                format: "dukascopy-bi5".into(),
                dataset: "EURUSD".into(),
                venue: "dukascopy".into(),
                server_dir: "/srv/vike-<unit>/market_data/imports/dukascopy-bi5/EURUSD".into(),
                dir: DatasetDir::Present,
                admission: "point value 100000 — vendor page, \"most FX pairs\"".into(),
                inventory: ArchiveInventory {
                    daily_files: 3,
                    first_day: Some(D0),
                    last_day: Some(D0 + 4 * DAY),
                    other_layout_days: vec![D0 - DAY],
                    other_objects: 2,
                    other_bytes: 1_234,
                    skipped: vec![
                        SkippedEntry {
                            path: "dukascopy-bi5/EURUSD/2024/00/16_ticks.bi5".into(),
                            class: EntryClass::Symlink,
                        },
                        SkippedEntry {
                            path: "dukascopy-bi5/EURUSD/2024/00/17_ticks.bi5".into(),
                            class: EntryClass::Fifo,
                        },
                    ],
                },
                from_day: Some(D0),
                to_day: Some(D0 + 4 * DAY),
                days: vec![
                    DayPlan {
                        day: D0,
                        class: DayClass::Free,
                        file_bytes: 20_298,
                        declared_ticks: Some(4_201),
                    },
                    DayPlan {
                        day: D0 + DAY,
                        class: DayClass::Supersede {
                            key: "dukascopy-provisional:EURUSD:x-y".into(),
                        },
                        file_bytes: 9,
                        declared_ticks: None,
                    },
                    DayPlan {
                        day: D0 + 3 * DAY,
                        class: DayClass::Overlapped { keys: vec!["dukascopy:EURUSD:a-b".into()] },
                        file_bytes: 7,
                        declared_ticks: Some(0),
                    },
                    DayPlan {
                        day: D0 + 4 * DAY,
                        class: DayClass::Refused(DayRefusal {
                            class: "MixedLayout".into(),
                            detail: "a daily file and hourly files for one day".into(),
                        }),
                        file_bytes: 1,
                        declared_ticks: None,
                    },
                ],
                gaps: vec![D0 + 2 * DAY],
                bars: vec!["1m".into()],
                series: Some(SeriesCoverage {
                    first_ts: D0,
                    last_ts: D0 + DAY,
                    rows: 10,
                    bytes: 126,
                    parts: 1,
                    dates: 1,
                }),
            },
            outcome: Some(ImportOutcome {
                days: vec![
                    DayOutcome {
                        day: D0,
                        result: DayResult::Imported {
                            ticks: 4_201,
                            bars: vec![BarsWritten { interval: "1m".into(), rows: 60 }],
                        },
                    },
                    DayOutcome { day: D0 + DAY, result: DayResult::ToppedUp { bars: vec![] } },
                    DayOutcome { day: D0 + 2 * DAY, result: DayResult::Verified { ticks: 3 } },
                    DayOutcome {
                        day: D0 + 3 * DAY,
                        result: DayResult::Refused(DayRefusal {
                            class: "AmbiguousTimeBase".into(),
                            detail: "every tick lies in the first hour".into(),
                        }),
                    },
                ],
            }),
        }
    }

    /// The request and a FULLY populated answer — every variant of every enum the plan and the outcome
    /// carry — survive `write_frame` -> `read_frame` whole.
    #[test]
    fn the_request_and_a_populated_answer_survive_the_frame_codec() {
        let done = populated_done();
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Request::ImportArchive(spec())).unwrap();
        write_frame(&mut buf, &Response::ArchiveImported(Box::new(done.clone()))).unwrap();
        // ...and the plan-only shape, with no outcome and an absent directory.
        let mut absent = done.clone();
        absent.plan.dir = DatasetDir::Absent;
        absent.outcome = None;
        write_frame(&mut buf, &Response::ArchiveImported(Box::new(absent.clone()))).unwrap();
        let mut unreadable = absent.clone();
        unreadable.plan.dir = DatasetDir::Unreadable { why: "permission denied".into() };
        write_frame(&mut buf, &Response::ArchiveImported(Box::new(unreadable.clone()))).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::ImportArchive(got) => assert_eq!(got, spec()),
            other => panic!("expected ImportArchive, got {other:?}"),
        }
        for want in [done, absent, unreadable] {
            match read_frame::<_, Response>(&mut cur).unwrap() {
                Response::ArchiveImported(got) => assert_eq!(*got, want),
                other => panic!("expected ArchiveImported, got {other:?}"),
            }
        }
    }

    #[test]
    fn only_free_and_supersede_days_are_importable() {
        let done = populated_done();
        assert_eq!(done.plan.importable_days(), 2, "{:?}", done.plan.days);
        assert!(!DayClass::TooRecent.is_importable());
        assert!(!DayClass::HeldByArchive.is_importable());
        assert!(!DayClass::HeldByHttp.is_importable());
    }
}
