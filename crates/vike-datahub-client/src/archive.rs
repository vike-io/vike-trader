//! **The ARCHIVE IMPORT verb's vocabulary** — the request, the answer, and the rules BOTH ends need
//! for [`Request::ImportArchive`](crate::proto::Request::ImportArchive) /
//! [`Response::ArchiveImported`](crate::proto::Response::ArchiveImported).
//!
//! Design: `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`; verdict:
//! `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md`.
//! The user downloads a vendor archive under THEIR OWN account into a folder on the DATAHUB's box,
//! and this verb reads one dataset of that folder into the served store, with a plan shown first;
//! no vendor credential ever enters vike. Declared ungated: a default `vike-datahub` build must
//! DECODE the verb in order to refuse it cleanly.
//!
//! # What the request names, and what it deliberately cannot
//!
//! - **A FORMAT** — a registry id the SERVER owns and advertises as one `import_format=<id>` entry
//!   per registered format ([`crate::proto::import_format_feature`]). The format decides the layout,
//!   time base, price scale, commit keys AND the venue, so the request names **no venue**.
//! - **ONE dataset directory NAME** — never a path. The server composes
//!   `<imports root>/<format>/<dataset>` itself, so [`validate_import_dataset`] is a SECURITY
//!   boundary rather than hygiene: the first argument on this wire that names a filesystem object.
//! - **An inclusive window of UTC DAYS**, each bound the epoch-ms START of a day, and the bar
//!   intervals to derive per imported day.
//! - **`dry_run` / `verify`** — the plan is ALWAYS computed and returned; `dry_run` decides only
//!   whether the import half runs, and `verify` (only beside `dry_run`) decodes every planned file
//!   while still writing nothing.
//!
//! # What the answer carries
//!
//! [`ImportDone`] has [`crate::proto::DeleteDone`]'s shape: one type for "what I would do" and "what
//! I did". The SERVER's plan is the authority — a client that previewed one and then executes gets a
//! re-plan. It carries counts, day indices, commit keys and paths RELATIVE to the imports root,
//! **never file bytes**; a day's refusal names a class and a sentence, never content.
//!
//! # Why the rules live HERE
//!
//! A client cannot guard a rule the server does not know, nor the reverse (the rule [`crate::seed`]
//! and [`crate::catalog`] state). [`validate_import_spec`] runs at BOTH doors —
//! [`crate::DatahubClient::import_archive`] before a frame is written, the data daemon's handler
//! before a directory is touched. The client copy is for the MESSAGE; the server copy is the
//! enforcement. Bounds that are the SERVER's alone (per-file caps, walk caps, the one-import slot)
//! stay with the server: a client that knew them could only mis-predict them.
//!
//! # Scope: Write, not Observe
//!
//! [`crate::proto::required_scope`] puts this verb in [`VerbScope::Write`](vike_hub_protocol::auth::VerbScope::Write) beside
//! `Backfill`, and its arm carries the argument: it WRITES the served store, the CLIENT names the
//! window (the fourth leg of `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`'s
//! decision 3 fails), it steers server-side filesystem reads, and a mis-scaled day spends its commit
//! key irreversibly. It removes nothing, so it takes `Backfill`'s posture: served on a key-less
//! LOOPBACK datahub, Control-only on a keyed one.

use serde::{Deserialize, Serialize};
use vike_data::SeriesCoverage;
// A day's refusal and its bar counts ride this wire as the store half mints them: one type each,
// `vike_data::archive_day`'s (its module doc has the serde contract).
use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::csv_bars::{CsvBarsFormat, CsvBarsFormatWords};
use vike_model::MS_PER_DAY;

/// The longest dataset name a request may carry (design §2.3): a dataset is ONE vendor instrument
/// folder, named by a short upper-case code (`EURUSD`). 32 bounds what a hostile frame can make the
/// server compose into a path before anything else runs.
pub const IMPORT_MAX_DATASET_BYTES: usize = 32;

/// The most UTC days ONE decoding request may cover — an import, or a `verify` dry run.
///
/// ⚠ **A bound on one SYNCHRONOUS request's duration, not on the total.** This wire has no progress
/// frame and a client's read of this verb's reply is unbounded by default
/// (`crates/vike-datahub-client/src/proto/verbs.rs`'s `reply_wait`), so a long decode is
/// indistinguishable from a dead server. 31 days is one calendar month at most: a client that splits
/// by MONTH never meets it, and a killed session resumes at the next month. A plan-only dry run
/// decodes nothing and is NOT bounded by it, so an operator can see the whole dataset first. The
/// server re-checks this cap after resolving an omitted bound, which the client cannot.
pub const IMPORT_MAX_DAYS: i64 = 31;

/// The most bar intervals ONE request may ask to derive per imported day — bounds the resample work
/// each day costs (design §5).
pub const IMPORT_MAX_BAR_INTERVALS: usize = 4;

/// The longest spelling of ONE bar interval a request may carry.
///
/// ⚠ **It protects the PARSER, not the classification.** `crates/vike-marketdata/src/interval.rs`'s
/// `interval_ms` multiplies a parsed `i64` count by the unit's width, so a nineteen-digit count
/// overflows (a panic in a debug build). Every day-dividing interval is at most six bytes
/// (`86400s`), so eight refuses nothing real.
pub const IMPORT_MAX_INTERVAL_BYTES: usize = 8;

/// **The ONE dataset validator, shared by both ends** — the argument on this wire that becomes a
/// PATH COMPONENT. The rules (design §2.3), cheapest first, each naming what was wrong:
///
/// 1. non-empty, and at most [`IMPORT_MAX_DATASET_BYTES`];
/// 2. the FIRST byte is `[A-Z0-9]` — refusing `.`, `..`, every leading dot, an absolute path and a
///    leading separator;
/// 3. EVERY byte is `[A-Z0-9._-]` — refusing every separator (`/`, `\`), a drive letter's `:`, NUL
///    and every other control byte, whitespace, and lower case;
/// 4. not a Windows reserved DEVICE NAME (`CON`, `NUL`, `COM1`…) under any extension.
///
/// ⚠ **Upper case only is a data rule too:** the dataset becomes the series SYMBOL, so admitting
/// `eurusd` beside `EURUSD` would give one instrument two series.
///
/// ⚠ **Rule 4 is DELEGATED** to `crates/vike-runs/src/lib.rs`'s `valid_mark_name`, the
/// workspace's one device-name list and stem rule (`CON` and `CON.X` alike). Rules 1–3 are strictly
/// narrower than everything else it checks, so a name reaching it can only fail as a device name;
/// `a_name_that_passes_the_charset_fails_the_mark_rule_only_as_a_device_name` holds that.
///
/// ⚠ **The message never ECHOES the dataset** (the rule `vike_hub_protocol::wire_ident::validate_seed_symbol` and
/// `vike_hub_protocol::wire_ident::validate_catalog_venue` state): a refusal quoting the untrusted string carries
/// it into a server log whose file layer defaults to `trace`. It names the LENGTH, the CAP and the
/// OFFSET.
///
/// ⚠ **Not trimmed, not upper-cased:** the server composes the path from the name it was sent, so
/// coercing it here would make the request and the directory read disagree.
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
    if vike_runs::valid_mark_name(dataset).is_err() {
        return Err(
            "an import dataset is a reserved Windows DEVICE NAME (`CON`, `PRN`, `AUX`, `NUL`, \
             `COM1`–`COM9`, `LPT1`–`LPT9`, under any extension). A folder called that cannot be \
             opened on Windows, so no dataset may be named after one."
                .to_string(),
        );
    }
    Ok(())
}

/// The bytes a dataset may be built from: upper-case ASCII letters, digits, `.`, `_` and `-`. An
/// ALLOWLIST: the set of dangerous bytes in a path component is open, the safe set is one line.
fn is_dataset_byte(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
}

/// The bar-interval rules: at most [`IMPORT_MAX_BAR_INTERVALS`], no repeat, each a CANONICAL
/// spelling of a step that DIVIDES a UTC day. An EMPTY list means "derive no bars" (`--bars none`).
///
/// - **Divides a UTC day** (design §3.5): bars are derived PER IMPORTED DAY, and a `7m` bucket would
///   straddle midnight. Such bars are derived later from the stored ticks.
/// - **Canonical — no leading zero, no zero count:** the string becomes the bar series' INTERVAL
///   partition, so `01m` beside `1m` would be two series of identical bars.
/// - **At most [`IMPORT_MAX_INTERVAL_BYTES`] before it is parsed** (see that constant).
/// - **No repeat:** a repeat asks for nothing new and would pad past [`IMPORT_MAX_BAR_INTERVALS`].
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
        match vike_marketdata::interval::canonical_bar_step_ms(iv) {
            Some(w) => {
                if MS_PER_DAY % w != 0 {
                    return Err(format!(
                        "bar interval {iv:?} does not divide a UTC day, so its buckets straddle \
                         midnight and a per-day import cannot build them. Import with a step that \
                         divides a day (`1m`, `5m`, `1h`, `1d`), and derive {iv:?} from the stored \
                         ticks afterwards."
                    ));
                }
            }
            None => {
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
/// ⚠ **An omitted bound is not refused and the day cap is not checked against it here:** `None`
/// means the dataset's first (or last) day, which only the server can resolve and re-check.
///
/// ⚠ **`verify` without `dry_run` is REFUSED:** beside an execute it would either be ignored (the
/// operator believes a verification ran) or turn the execute into a dry run (the operator believes
/// an import ran). An input whose meaning is uncertain is refused before a key is spent.
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
        // Saturating: two far-apart aligned bounds would overflow a plain subtraction — a panic on
        // a server thread, from a request. Saturated, the span is huge and refused below.
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

/// **Every client-checkable rule on one request** — the dataset, the bars and the window, cheapest
/// first. Called at BOTH doors (see the module doc).
///
/// ⚠ The FORMAT is not validated here: it is matched by exact equality against the server's
/// registry (and, client-side, its advertisement), so an unregistered id is refused BY that lookup,
/// naming what IS registered.
pub fn validate_import_spec(spec: &ImportSpec) -> Result<(), String> {
    validate_import_dataset(&spec.dataset)?;
    validate_import_bars(&spec.bars)?;
    if let Some(target) = &spec.target {
        validate_import_target(target)?;
    }
    validate_import_window(spec.from_day, spec.to_day, spec.dry_run, spec.verify)
}

/// One [`Request::ImportArchive`](crate::proto::Request::ImportArchive): import ONE dataset of one
/// archive FORMAT from the SERVER's own imports directory into the store.
///
/// The design sketches a struct VARIANT; a newtype variant over this struct is the SAME BYTES
/// (`{"ImportArchive":{"format":…,…}}`), pinned by `the_import_frame_has_the_designed_shape`.
///
/// ⚠ **`dry_run` has NO serde default, deliberately.** With one, a frame that dropped it would
/// decode as `false` — an IMPORT — and write. Without, it fails to DECODE and is answered
/// `Response::Error`; `a_frame_that_omits_dry_run_does_not_decode` holds that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSpec {
    /// A registry id the server advertised as `import_format=<id>` — `"dukascopy-bi5"`. It is also
    /// the directory name under the imports root, and it decides the venue.
    pub format: String,
    /// ONE path segment under `<imports root>/<format>/` — `"EURUSD"`. See
    /// [`validate_import_dataset`]. The format decides the series SYMBOL from it: `dukascopy-bi5`
    /// lands `EURUSD` as is, and `binance-aggtrades` lands `SOLUSDT` as the perpetual `SOLUSDT.P`.
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
    /// Replace a day another producer holds whose stored rows are fewer than a whole day's with the
    /// archive's verified day, and record the day's owner anew (decision 0268, point 4): such a day
    /// plans as [`ArchiveDayClassWire::Replace`]. A whole day is never replaced, and a format that cannot judge
    /// a whole day refuses the request. A dry run lists the days; only an import replaces them.
    ///
    /// Left off the frame when `false`, so a request that does not ask is byte-identical to one from
    /// before the field; a client sends `true` only to a server advertising
    /// [`crate::proto::FEATURE_IMPORT_REPLACE_HOLED`], since an older one would ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_holed: bool,
    /// The series a format that cannot name its own writes into: `csv-bars`, a user's file, whose
    /// venue, symbol and interval are the operator's word and whose rows land only in the lane
    /// `user-<label>` (decision 0268, point 3). `None` for every format that decides its series from
    /// the dataset; a server refuses a target such a format was sent, and a target-taking format
    /// refuses a request without one. See [`validate_import_target`].
    ///
    /// Left off the frame when `None`, so a request without one is byte-identical to one from before
    /// the field. No feature string: only a server that advertises `import_format=csv-bars` is ever
    /// sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ImportTarget>,
}

/// Where a `csv-bars` import writes: [`ImportSpec::target`], and an upload's
/// ([`crate::upload::CsvUploadSpec::target`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportTarget {
    /// A venue of the roster (`vike_model::VENUES`), or the reserved
    /// `vike_data::store::store_kind::USER_VENUE` for data that is no venue's (decision 0272): the
    /// store's `venue=`.
    pub venue: String,
    /// The instrument, in the store's spelling (`BTCUSDT.P`).
    pub symbol: String,
    /// The bars' interval: a canonical step that divides a UTC day. The file must be at that step.
    pub interval: String,
    /// The lane's label: the rows land in `user-<label>`, by
    /// `vike_data::store::store_kind::user_lane`'s grammar.
    pub label: String,
    /// How the file is written, when not in the decoder's plain form, in the decoder's own words
    /// (each `None` for its default; nothing on either side reads a machine's locale, and
    /// `vike_data::csv_bars::CsvBarsFormat::parse` builds the format at both doors). Left off the frame
    /// when plain.
    #[serde(default, skip_serializing_if = "CsvBarsFormatWords::is_plain")]
    pub file_format: CsvBarsFormatWords,
    /// Which column is which field, in `vike_data::csv_bars::CsvBarsColumns::parse`'s words
    /// (`time=timestamp_ms,close=value`), or `None`: found by the header's names. Left off the frame when
    /// `None`, so a request without one is byte-identical to one from before the field; a client
    /// sends one only to a server advertising [`crate::proto::FEATURE_CSV_COLUMNS`], since an older
    /// one would ignore it (decision 0272).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<String>,
}

impl ImportTarget {
    /// The decoder's mapping [`Self::columns`] names: the header's names when it names none.
    pub fn columns(&self) -> Result<vike_data::csv_bars::CsvBarsColumns, String> {
        match &self.columns {
            None => Ok(vike_data::csv_bars::CsvBarsColumns::Header),
            Some(words) => vike_data::csv_bars::CsvBarsColumns::parse(words),
        }
    }
}

/// The target rules, cheapest first: a venue of the roster or the reserved user venue, a symbol of the
/// seed verb's charset, a canonical interval that divides a UTC day, a label the user lane's grammar
/// admits, a file format the decoder can build and a column mapping it can read. Run at both doors by
/// [`validate_import_spec`] and the upload's own validator, so a bad label or a bad format word is
/// refused before a file is read.
pub fn validate_import_target(target: &ImportTarget) -> Result<(), String> {
    let user = vike_data::store::store_kind::USER_VENUE;
    if target.venue != user && !vike_model::VENUES.contains(&target.venue.as_str()) {
        return Err(format!(
            "an import target's venue is neither `{user}` (data that is no venue's) nor one of the \
             roster: [{}]. The rows land under it, so it must be a venue this workspace knows.",
            vike_model::VENUES.join(", ")
        ));
    }
    vike_hub_protocol::wire_ident::validate_seed_symbol(&target.symbol)?;
    if target.symbol.starts_with('.') {
        return Err("an import target's symbol may not open with `.`.".to_string());
    }
    let step = vike_marketdata::interval::canonical_bar_step_ms(&target.interval)
        .filter(|_| vike_marketdata::interval::measures_bar_step(&target.interval));
    if !step.is_some_and(|step| MS_PER_DAY % step == 0) {
        return Err(format!(
            "an import target's interval {:?} is not a canonical step that divides a UTC day \
             (`1m`, `5m`, `1h`, `1d`): the rows are stored one UTC day at a time.",
            target.interval
        ));
    }
    vike_data::store::store_kind::user_lane(&target.label)?;
    CsvBarsFormat::parse(&target.file_format).map_err(|e| format!("the file's format: {e}"))?;
    // Bounded before it is parsed: a refusal of a mapping names its words.
    if target.columns.as_ref().is_some_and(|c| c.len() > IMPORT_MAX_COLUMNS_BYTES) {
        return Err(format!(
            "an import target's column mapping is over IMPORT_MAX_COLUMNS_BYTES = \
             {IMPORT_MAX_COLUMNS_BYTES}: six `field=header-name` pairs fit in far less."
        ));
    }
    target.columns().map(|_| ()).map_err(|e| format!("the file's columns: {e}"))
}

/// The longest column mapping ([`ImportTarget::columns`]) a request may carry.
pub const IMPORT_MAX_COLUMNS_BYTES: usize = 512;

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
/// (`dry_run` without `verify`); a `verify` dry run's days are [`ArchiveDayResultWire::Verified`] or
/// [`ArchiveDayResultWire::Refused`], and it writes nothing.
///
/// ⚠ A WHOLE-REQUEST refusal (unknown format, invalid dataset, an instrument the format cannot
/// scale, a window over the day cap, a second concurrent import) is `Response::Error`, because
/// nothing was planned. A refused DAY is a value inside the plan or the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDone {
    /// What the server found and decided, day by day. The SERVER's plan is the authority.
    pub plan: ImportPlan,
    /// What it did. `None` iff the request was a plan-only dry run.
    pub outcome: Option<ImportOutcome>,
}

/// Whether the dataset's directory could be walked. Three states, because "absent" (a sync that has
/// not happened, or happened on the wrong box) and "exists, cannot be read" (a permission, or a
/// sandbox such as a unit's `ProtectHome`) call for different fixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportDatasetDirectory {
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
/// Every day is an [`ImportPlanDay`] carrying its classification; every field below is a COUNT, a day
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
    /// The dataset directory ON THE DATAHUB'S BOX, as the server resolved it: a tunnelled datahub is
    /// `127.0.0.1` too, so this is how an operator learns WHICH box the files must be on.
    pub server_dir: String,
    /// Whether that directory could be walked.
    pub dir: ImportDatasetDirectory,
    /// The format's own one-line statement of why this dataset is admitted — for Dukascopy's daily
    /// files, the instrument's point value and the evidence for it. A dataset the format cannot
    /// admit never gets a plan: that is a whole-request refusal.
    pub admission: String,
    /// What the walk found in the whole dataset, whatever the window.
    pub inventory: ImportPlanInventory,
    /// The RESOLVED inclusive first day — the request's bound, or the dataset's first daily-file day
    /// when it omitted one. `None` only when it omitted one and the dataset holds no daily file.
    pub from_day: Option<i64>,
    /// The RESOLVED inclusive last day, by the same rule.
    pub to_day: Option<i64>,
    /// Every day inside the resolved window that HAS a daily file, ascending, each classified. A day
    /// with no file is never here — see [`Self::gaps`].
    pub days: Vec<ImportPlanDay>,
    /// Weekdays inside both the resolved window and the dataset's own span that hold NO daily file,
    /// ascending. What a missing file means is the vendor's (Dukascopy's says no ticks were recorded),
    /// or the sync has not fetched it yet; a gap spends nothing, so a later sync fills it.
    pub gaps: Vec<i64>,
    /// The intervals that will be derived per imported day, echoed from the request.
    pub bars: Vec<String>,
    /// The quote series' coverage as the store holds it NOW, from either lane, or `None` when it
    /// holds nothing. A client extrapolates the store growth from its `bytes` / `rows`; with `None`
    /// the growth is unknown until the first month lands, and a plan must say so rather than guess.
    pub series: Option<SeriesCoverage>,
    /// The first rows of the dataset AS THE FORMAT READ THEM, for a format that reads its files at
    /// plan time (`csv-bars`: the first [`IMPORT_PREVIEW_ROWS`] bars of its first file by name), so a
    /// user sees how a file was understood before anything is written. Empty for every other format.
    /// A pure addition to the reply: an older server leaves it out and it decodes as empty, and an
    /// older client ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preview: Vec<ImportPlanPreviewRow>,
}

/// How many rows [`ImportPlan::preview`] carries.
pub const IMPORT_PREVIEW_ROWS: usize = 3;

/// One row of [`ImportPlan::preview`]: the bar's open time and its five numbers, each in the shortest
/// spelling that reads back as the value the decoder parsed. Text rather than `f64`, so the plan
/// stays `Eq` and a client prints exactly what the server read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlanPreviewRow {
    /// The bar's open time, epoch ms UTC.
    pub timestamp: i64,
    /// `open`, `high`, `low`, `close`, `volume`.
    pub open: String,
    /// See [`Self::open`].
    pub high: String,
    /// See [`Self::open`].
    pub low: String,
    /// See [`Self::open`].
    pub close: String,
    /// See [`Self::open`].
    pub volume: String,
}

impl ImportPlanPreviewRow {
    /// `bar` as a preview row.
    pub fn of(bar: &vike_marketdata::Bar) -> Self {
        ImportPlanPreviewRow {
            timestamp: bar.timestamp,
            open: bar.open.to_string(),
            high: bar.high.to_string(),
            low: bar.low.to_string(),
            close: bar.close.to_string(),
            volume: bar.volume.to_string(),
        }
    }
}

impl ImportPlan {
    /// How many days the import half would store — [`ArchiveDayClassWire::Free`] plus
    /// [`ArchiveDayClassWire::Supersede`]. The number a typed confirmation names.
    pub fn importable_days(&self) -> usize {
        self.days.iter().filter(|d| d.class.is_importable()).count()
    }
}

/// What the walk found in the dataset, over its WHOLE span.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlanInventory {
    /// Daily files of the importable layout.
    pub daily_files: u64,
    /// The first and last day holding a daily file, `None` when there is none.
    pub first_day: Option<i64>,
    /// See [`Self::first_day`].
    pub last_day: Option<i64>,
    /// Days that hold files of a RECOGNISED layout this format does not import (for Dukascopy, the
    /// hourly layout) and no daily file, ascending. Counted and reported, never imported. A day
    /// holding BOTH layouts is not here: it is a refused [`ImportPlanDay`].
    pub other_layout_days: Vec<i64>,
    /// Entries with an unexpected name or at an unexpected depth — counted with their bytes and
    /// never opened.
    pub other_objects: u64,
    /// The bytes of [`Self::other_objects`].
    pub other_bytes: u64,
    /// Entries the walk refused to follow or open — see [`ImportPlanEntryClass`]. Each is named RELATIVE to
    /// the imports root. Bounded by the server's own walk cap.
    pub skipped: Vec<ImportPlanSkippedEntry>,
}

/// One entry the walk classified and SKIPPED, never opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlanSkippedEntry {
    /// The entry's path RELATIVE to the imports root — never an absolute path, never content.
    pub path: String,
    /// Why it was skipped.
    pub class: ImportPlanEntryClass,
}

/// Why the walk skipped an entry. The walk never FOLLOWS anything: only plain directories and plain
/// files with the expected names are accepted, and every other object becomes a refused entry rather
/// than a read outside the root or a hung thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportPlanEntryClass {
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
pub struct ImportPlanDay {
    /// The epoch-ms START of the UTC day.
    pub day: i64,
    /// What the import does with it.
    pub class: ArchiveDayClassWire,
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
pub enum ArchiveDayClassWire {
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
    Refused(ArchiveDayRefusal),
    /// Only under [`ImportSpec::replace_holed`]: a day another producer holds ([`Self::HeldByHttp`]
    /// or [`Self::Overlapped`]) whose stored rows are fewer than a whole day's. The import replaces
    /// the whole day with the archive's verified file and its day key owns the day from then on.
    /// A server sends it only to a request that asked, so an older client never meets it.
    Replace {
        /// The commit keys that met the day.
        keys: Vec<String>,
        /// Distinct rows the store holds for the day.
        stored: u64,
        /// Rows a whole day holds at the series' interval.
        full: u64,
    },
}

impl ArchiveDayClassWire {
    /// Whether the import half STORES this day — [`Self::Free`], [`Self::Supersede`] and
    /// [`Self::Replace`].
    pub fn is_importable(&self) -> bool {
        matches!(self, Self::Free | Self::Supersede { .. } | Self::Replace { .. })
    }
}

/// What the import half did — present on an import and on a `verify` dry run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcome {
    /// One row per day the import half acted on, ascending.
    pub days: Vec<ImportOutcomeDay>,
}

/// One day's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOutcomeDay {
    /// The epoch-ms START of the UTC day.
    pub day: i64,
    /// What happened to it.
    pub result: ArchiveDayResultWire,
}

/// What happened to one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchiveDayResultWire {
    /// Decoded and stored: `ticks` quotes under the archive day key, and the bars derived from them.
    Imported {
        /// Ticks stored.
        ticks: u64,
        /// Bars written per requested interval.
        bars: Vec<ArchiveDayBars>,
        /// The same sentence [`Self::Verified`]'s `report` carries, for the day this import stored:
        /// what the format found in the file and what the import did with it (`binance-aggtrades`:
        /// exact repeats dropped, ids that do not run on); `None` for a clean file. Added the same
        /// way and for the same reason: an older server leaves it out and it decodes as `None`, and
        /// an older client ignores it and reads the day as imported, which it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<String>,
    },
    /// A HELD day: nothing decoded, and the requested intervals that were missing derived from the
    /// stored ticks. An empty list means none was missing.
    ToppedUp {
        /// Bars written per interval that was missing.
        bars: Vec<ArchiveDayBars>,
    },
    /// A `verify` dry run decoded the file and an import would store it. Nothing was written.
    Verified {
        /// Ticks the file decoded to.
        ticks: u64,
        /// What the format found in the file that refuses no day, and what an import does with it, as
        /// an operator's sentence (`binance-aggtrades`: ids or times that do not run on, stored as
        /// published, and exact repeats of a row, dropped); `None` for a clean file. A pure
        /// addition to a reply, so no feature string and no version bump
        /// (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`,
        /// verdict 3): an older server leaves it out and it decodes as `None`, and an older client
        /// ignores it and reads the day as verified, which is what an import does with it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<String>,
    },
    /// Refused while decoding or storing. No commit key was spent for it, and the request carried on
    /// to the next day.
    Refused(ArchiveDayRefusal),
}

#[path = "archive_tests.rs"]
#[cfg(test)]
mod archive_tests;
